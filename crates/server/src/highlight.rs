//! Syntax highlighting for the file a student has on screen.
//!
//! Done here rather than in the browser, for three reasons: the dashboard
//! vendors its JavaScript by hand and adding a highlighter plus its language
//! packs to `static/vendor/` is a much larger dependency than a crate; a
//! snapshot is highlighted once on arrival instead of once per teacher per
//! poll; and the result is plain data, so the page keeps control of escaping
//! and of splicing the student's caret into the right place.
//!
//! What comes out is **spans, not colours**. Each line is a list of
//! `(class, text)` pairs drawn from a deliberately tiny vocabulary, and the
//! page maps those classes onto its own design tokens — so highlighting
//! follows the chalkboard/whiteboard themes instead of dragging a third
//! palette in behind them.
//!
//! The interface is shaped so the awkward cases can't be reached: you can only
//! highlight with a [`Grammar`], and the only way to get one is to have found a
//! language for the file, so "unknown language" is settled before any parsing
//! is asked for. A class is a [`Class`], not a string, so the page can never be
//! sent one it has no colour for.

use std::sync::OnceLock;

use serde::{Serialize, Serializer};
use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Buffers longer than this aren't highlighted. Parsing is linear and cheap,
/// but a snapshot is rebuilt on every keystroke burst and the spans travel to
/// the browser each poll; past a few thousand lines that costs more than it
/// buys.
const MAX_LINES: usize = 4000;

/// Why a buffer came back plain rather than coloured.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("more than {} lines", MAX_LINES)]
    TooLong,
    /// The syntax definition failed partway. Plain text is better than a file
    /// that is half-wrong colour.
    #[error("the syntax definition could not parse the input")]
    Syntax,
}

/// What a run of text is, as far as colouring it goes.
///
/// Serialized as a single letter (`""` for [`Class::Plain`]) because a
/// snapshot carries one per span and the names would outweigh the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Class {
    #[serde(rename = "")]
    Plain,
    #[serde(rename = "c")]
    Comment,
    #[serde(rename = "s")]
    String,
    #[serde(rename = "n")]
    Number,
    #[serde(rename = "k")]
    Keyword,
    #[serde(rename = "f")]
    Function,
    #[serde(rename = "t")]
    Type,
    #[serde(rename = "o")]
    Operator,
}

/// A run of characters sharing a class. Serialized as a two-element array,
/// `["k","return"]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    class: Class,
    text: String,
}

impl Span {
    pub fn new(class: Class, text: impl Into<String>) -> Self {
        Self {
            class,
            text: text.into(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// For tests to read a span back; nothing outside them needs to.
    #[cfg(test)]
    pub fn class(&self) -> Class {
        self.class
    }
}

impl Serialize for Span {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.class, self.text.as_str()).serialize(serializer)
    }
}

/// The spans of one line. Concatenated, they are exactly that line's text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Line(Vec<Span>);

impl Line {
    /// A line with nothing to say about it: one classless span, or none at all
    /// for an empty line.
    pub fn plain(text: &str) -> Self {
        if text.is_empty() {
            Self::default()
        } else {
            Self(vec![Span::new(Class::Plain, text)])
        }
    }

    /// For tests to read spans back; nothing outside them needs to.
    #[cfg(test)]
    pub fn spans(&self) -> &[Span] {
        &self.0
    }

    /// Whether these spans are exactly `text`. Spans are looked up by line
    /// number, so this is what proves a lookup landed on the right line.
    pub fn rebuilds(&self, text: &str) -> bool {
        let mut rest = text;
        for span in &self.0 {
            match rest.strip_prefix(span.text()) {
                Some(after) => rest = after,
                None => return false,
            }
        }
        rest.is_empty()
    }
}

/// Every line of a highlighted buffer, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Highlighted(Vec<Line>);

impl Highlighted {
    /// The line at a 1-based screen line number.
    pub fn line(&self, number: u32) -> Option<&Line> {
        self.0.get(usize::try_from(number.checked_sub(1)?).ok()?)
    }
}

impl IntoIterator for Highlighted {
    type Item = Line;
    type IntoIter = std::vec::IntoIter<Line>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// A language to highlight in. Holding one is proof a language was found.
#[derive(Clone, Copy)]
pub struct Grammar(&'static SyntaxReference);

impl Grammar {
    /// Picks a grammar from what the editor called the language, then from the
    /// file's extension. `None` means the file renders plain.
    pub fn detect(language: &str, path: &str) -> Option<Self> {
        let ps = syntax_set();
        alias(language)
            .and_then(|name| ps.find_syntax_by_name(name))
            .or_else(|| ps.find_syntax_by_token(language))
            .or_else(|| ps.find_syntax_by_extension(path.rsplit('.').next()?))
            .map(Self)
    }

    /// Highlights lines into one [`Line`] each. The lines are split exactly as
    /// the browser splits them, so line *n* of the result is line *n* on
    /// screen — anything else would put the cursor on the wrong row.
    pub fn highlight<'a>(
        &self,
        lines: impl IntoIterator<Item = &'a str>,
    ) -> Result<Highlighted, Error> {
        let ps = syntax_set();
        let lines = lines.into_iter();
        // A caller that knows its length up front — a slice, a vector — is
        // refused before a single line is parsed, rather than after parsing
        // MAX_LINES of them to find out.
        if lines.size_hint().0 > MAX_LINES {
            return Err(Error::TooLong);
        }
        let mut state = ParseState::new(self.0);
        let mut stack = ScopeStack::new();
        let mut out = Vec::new();
        let mut buf = String::new();

        for line in lines {
            if out.len() == MAX_LINES {
                return Err(Error::TooLong);
            }
            // The parser wants the newline (some syntaxes end a context on it),
            // but it must not reach the page: the page draws the line break.
            buf.clear();
            buf.push_str(line);
            buf.push('\n');
            let ops = state.parse_line(&buf, ps).map_err(|_| Error::Syntax)?;

            let mut spans: Vec<Span> = Vec::new();
            for (text, op) in ScopeRegionIterator::new(&ops, &buf) {
                stack.apply(op).map_err(|_| Error::Syntax)?;
                let text = text.trim_end_matches('\n');
                if text.is_empty() {
                    continue;
                }
                let class = classify(&stack);
                // Runs that share a class are merged, which roughly halves the
                // span count on ordinary code.
                match spans.last_mut() {
                    Some(last) if last.class == class => last.text.push_str(text),
                    _ => spans.push(Span::new(class, text)),
                }
            }
            out.push(Line(spans));
        }
        Ok(Highlighted(out))
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// VSCode language ids that name no syntect syntax. Mapping them to the nearest
/// one beats falling back to no highlighting at all — TypeScript read as
/// JavaScript is wrong only about type annotations.
///
/// Only ids that resolve to nothing are listed: `find_syntax_by_token` already
/// matches by extension and by case-insensitive name, so `bash`, `objective-c`
/// and friends find their syntax unaided.
fn alias(language: &str) -> Option<&'static str> {
    Some(match language {
        "typescript" | "typescriptreact" | "javascriptreact" => "JavaScript",
        "shellscript" => "Bourne Again Shell (bash)",
        "objective-cpp" => "Objective-C++",
        "jsonc" | "json5" => "JSON",
        _ => return None,
    })
}

/// The highlighter's whole vocabulary, compiled once into syntect's interned
/// scopes. Kept small on purpose: the file pane is for reading someone's work
/// over their shoulder, so the useful distinctions are "this is prose", "this is
/// a literal", "this is a name" — not the fifty scopes a full theme separates.
///
/// Order matters, most specific first: `keyword.operator` must be matched
/// before `keyword`, which is a prefix of it.
fn rules() -> &'static [(Scope, Class)] {
    static RULES: OnceLock<Vec<(Scope, Class)>> = OnceLock::new();
    RULES.get_or_init(|| {
        use Class::*;
        [
            ("comment", Comment),
            ("string", String),
            ("constant", Number),
            ("keyword.operator", Operator),
            ("keyword", Keyword),
            ("storage", Keyword),
            ("entity.name.function", Function),
            ("support.function", Function),
            ("variable.function", Function),
            ("entity.name.type", Type),
            ("entity.name.class", Type),
            ("entity.name.struct", Type),
            ("support.type", Type),
            ("support.class", Type),
            ("entity.name.tag", Type),
        ]
        .into_iter()
        .filter_map(|(prefix, class)| Scope::new(prefix).ok().map(|s| (s, class)))
        .collect()
    })
}

/// The class for a scope stack: the most specific scope that we have an opinion
/// about, searched innermost-out. An inner `punctuation.definition` inside a
/// string is still string-coloured, which is what a reader expects.
///
/// `is_prefix_of` compares interned atoms, so this runs once per scope with no
/// allocation — `Scope::build_string` would take a global lock and build a
/// `String` for every token on the page.
fn classify(stack: &ScopeStack) -> Class {
    for scope in stack.scopes.iter().rev() {
        for (prefix, class) in rules() {
            if prefix.is_prefix_of(*scope) {
                return *class;
            }
        }
    }
    Class::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python() -> Grammar {
        Grammar::detect("python", "x.py").expect("python")
    }

    fn lines(source: &str) -> Vec<&str> {
        source.split('\n').collect()
    }

    /// Every line's spans must concatenate back to that exact line, or the page
    /// would render text the student never typed.
    fn assert_roundtrip(source: &str, grammar: Grammar) -> Highlighted {
        let highlighted = grammar.highlight(lines(source)).expect("highlighted");
        let count = lines(source).len() as u32;
        for (n, text) in lines(source).into_iter().enumerate() {
            let line = highlighted
                .line(n as u32 + 1)
                .expect("a line per screen line");
            assert!(line.rebuilds(text), "line {}: {line:?} != {text:?}", n + 1);
        }
        assert!(highlighted.line(count + 1).is_none());
        highlighted
    }

    #[test]
    fn classes_the_parts_a_reader_cares_about() {
        let c = Grammar::detect("c", "x.c").unwrap();
        let h = assert_roundtrip(
            "/* hi */\nint main(void) {\n    char *s = \"x\";\n    return 0;\n}\n",
            c,
        );
        let class_of = |line: u32, text: &str| {
            h.line(line)
                .unwrap()
                .spans()
                .iter()
                .find(|s| s.text().contains(text))
                .unwrap_or_else(|| panic!("{text:?} on line {line}"))
                .class()
        };
        assert_eq!(class_of(1, "hi"), Class::Comment);
        assert_eq!(
            class_of(2, "int"),
            Class::Keyword,
            "storage reads as a keyword"
        );
        assert_eq!(class_of(2, "main"), Class::Function);
        assert_eq!(class_of(3, "x"), Class::String);
        assert_eq!(class_of(4, "return"), Class::Keyword);
        assert_eq!(class_of(4, "0"), Class::Number);
    }

    #[test]
    fn line_count_matches_a_browser_split() {
        // A trailing newline means a final empty line on screen; Rust's
        // `lines()` would silently drop it and shift every later cursor.
        let h = python().highlight(lines("a = 1\nb = 2\n")).unwrap();
        assert!(h.line(3).is_some_and(|l| l.spans().is_empty()));
        assert!(h.line(4).is_none());
        assert!(h.line(0).is_none(), "lines are 1-based");
    }

    #[test]
    fn rebuilds_unicode_and_indentation_exactly() {
        assert_roundtrip(
            "s = \"héllo — wörld\"\n\tif True:\n        pass\n",
            python(),
        );
    }

    #[test]
    fn a_language_is_found_by_name_then_by_extension_or_not_at_all() {
        assert!(Grammar::detect("python", "notes").is_some());
        assert!(Grammar::detect("nonesuch", "ex1/main.py").is_some());
        assert!(Grammar::detect("nonesuch", "a.nonesuch").is_none());
    }

    #[test]
    fn language_ids_without_a_syntax_borrow_the_nearest() {
        for (id, path) in [
            ("typescript", "a.ts"),
            ("shellscript", "run"),
            ("jsonc", "a.jsonc"),
        ] {
            assert!(Grammar::detect(id, path).is_some(), "{id}");
        }
        let ts = Grammar::detect("typescript", "a.ts").unwrap();
        let h = ts.highlight(["const x: number = 1;"]).unwrap();
        assert!(h
            .line(1)
            .unwrap()
            .spans()
            .iter()
            .any(|s| s.class() == Class::Keyword && s.text().contains("const")));
    }

    #[test]
    fn very_long_buffers_are_refused_rather_than_partly_coloured() {
        let long = "x = 1\n".repeat(MAX_LINES);
        assert_eq!(python().highlight(lines(&long)), Err(Error::TooLong));
        let fits = "x = 1\n".repeat(MAX_LINES - 1);
        assert!(python().highlight(lines(&fits)).is_ok());
    }

    #[test]
    fn a_length_known_up_front_is_refused_before_anything_is_parsed() {
        /// Claims more lines than the cap, and fails the test if asked for one.
        struct NeverRead;
        impl Iterator for NeverRead {
            type Item = &'static str;
            fn next(&mut self) -> Option<&'static str> {
                panic!("a line was parsed before the length was checked");
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                (MAX_LINES + 1, Some(MAX_LINES + 1))
            }
        }
        assert_eq!(python().highlight(NeverRead), Err(Error::TooLong));
    }

    #[test]
    fn a_line_is_told_apart_from_its_neighbours() {
        let h = python().highlight(["x = 1", "y = 2"]).unwrap();
        assert!(h.line(1).unwrap().rebuilds("x = 1"));
        assert!(!h.line(1).unwrap().rebuilds("y = 2"));
        assert!(!h.line(2).unwrap().rebuilds("y = 2 "));
        assert!(Line::plain("").rebuilds(""));
    }

    #[test]
    fn serializes_to_the_compact_wire_shape() {
        let line = Line(vec![
            Span::new(Class::Keyword, "return"),
            Span::new(Class::Plain, " x"),
        ]);
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            r#"[["k","return"],[""," x"]]"#
        );
    }
}
