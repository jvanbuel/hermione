//! Syntax highlighting for the file a student has on screen.
//!
//! Done here rather than in the browser, for three reasons: the dashboard
//! vendors its JavaScript by hand and adding a highlighter plus its language
//! packs to `static/vendor/` is a much larger dependency than a crate; a
//! snapshot is highlighted once on arrival instead of once per teacher per
//! poll; and the result is plain data, so the page keeps control of escaping
//! and of splicing the student's caret into the right place.
//!
//! What comes out is **spans, not colours**. Each line becomes a list of
//! `(class, text)` pairs drawn from a deliberately tiny vocabulary, and the
//! page maps those classes onto its own design tokens — so highlighting
//! follows the chalkboard/whiteboard themes instead of dragging a third
//! palette in behind them.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Files longer than this aren't highlighted. Parsing is linear and cheap, but
/// a snapshot is rebuilt on every keystroke burst and the tokens travel to the
/// browser each poll; past a few thousand lines that costs more than it buys.
const MAX_LINES: usize = 4000;

/// One run of characters sharing a class. Serialized as a two-element array
/// (`["k","return"]`) because a snapshot carries one per token and the field
/// names would outweigh the data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Token(pub String, pub String);

/// The highlighter's whole vocabulary. Kept small on purpose: the file pane is
/// for reading someone's work over their shoulder, so the useful distinctions
/// are "this is prose", "this is a literal", "this is a name" — not the fifty
/// scopes a full theme separates.
fn class_for(scope: &str) -> Option<&'static str> {
    // Longest-prefix first: `keyword.operator` must not be read as `keyword`.
    const RULES: &[(&str, &str)] = &[
        ("comment", "c"),
        ("string", "s"),
        ("constant.numeric", "n"),
        ("constant", "n"),
        ("keyword.operator", "o"),
        ("keyword", "k"),
        ("storage", "k"),
        ("entity.name.function", "f"),
        ("support.function", "f"),
        ("entity.name.type", "t"),
        ("entity.name.class", "t"),
        ("entity.name.struct", "t"),
        ("support.type", "t"),
        ("support.class", "t"),
        ("entity.name.tag", "t"),
        ("variable.function", "f"),
    ];
    RULES
        .iter()
        .find(|(prefix, _)| scope == *prefix || scope.starts_with(&format!("{prefix}.")))
        .map(|(_, class)| *class)
}

/// The class for a scope stack: the most specific scope that we have an
/// opinion about, searched innermost-out. An inner `punctuation.definition`
/// inside a string is still string-coloured, which is what a reader expects.
fn classify(stack: &ScopeStack) -> &'static str {
    for scope in stack.scopes.iter().rev() {
        let name = scope.build_string();
        if let Some(class) = class_for(&name) {
            return class;
        }
    }
    ""
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    // ~2 MB of Sublime syntax definitions, unpacked once per process.
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// VSCode language ids that name no syntect syntax. Mapping them to the
/// nearest one beats falling back to no highlighting at all — TypeScript read
/// as JavaScript is wrong only about type annotations.
fn alias(language: &str) -> Option<&'static str> {
    Some(match language {
        "typescript" | "typescriptreact" | "javascriptreact" => "JavaScript",
        "shellscript" | "bash" | "sh" | "zsh" => "Shell-Unix-Generic",
        "objective-c" => "Objective-C",
        "objective-cpp" => "Objective-C++",
        "jsonc" | "json5" => "JSON",
        "restructuredtext" => "reStructuredText",
        _ => return None,
    })
}

/// Picks a syntax from what the editor told us, then from the file extension.
fn syntax<'a>(
    ps: &'a SyntaxSet,
    language: Option<&str>,
    path: Option<&str>,
) -> Option<&'a SyntaxReference> {
    if let Some(language) = language {
        if let Some(name) = alias(language) {
            if let Some(s) = ps.find_syntax_by_name(name) {
                return Some(s);
            }
        }
        if let Some(s) = ps.find_syntax_by_token(language) {
            return Some(s);
        }
    }
    let ext = path?.rsplit('.').next()?;
    ps.find_syntax_by_extension(ext)
}

/// Highlights a buffer into one token list per line.
///
/// The lines are split exactly as the browser splits them (`split('\n')`), so
/// line *n* of the result is line *n* on screen — anything else would put the
/// cursor highlight on the wrong row. Returns `None` when the language is
/// unknown or the file is too long, and the page then renders it plain.
pub fn highlight(
    content: &str,
    language: Option<&str>,
    path: Option<&str>,
) -> Option<Vec<Vec<Token>>> {
    let ps = syntax_set();
    let syntax = syntax(ps, language, path)?;
    if content.split('\n').count() > MAX_LINES {
        return None;
    }
    spans_for(syntax, ps, content.split('\n'))
}

/// Highlights a run of lines that isn't a whole file.
///
/// Used for the removed side of a diff hunk, which exists only in the
/// student's last commit and so appears nowhere in the buffer we were sent.
/// Parsing starts from a clean state, so a fragment that begins inside a
/// multi-line construct can be mis-scoped — pass the hunk's context lines
/// along with the removed ones to give the parser what context there is.
pub fn highlight_lines(
    lines: &[String],
    language: Option<&str>,
    path: Option<&str>,
) -> Option<Vec<Vec<Token>>> {
    let ps = syntax_set();
    let syntax = syntax(ps, language, path)?;
    if lines.len() > MAX_LINES {
        return None;
    }
    spans_for(syntax, ps, lines.iter().map(String::as_str))
}

/// The parse loop shared by both entry points: one token list per input line,
/// carrying parser state across lines so multi-line constructs stay coherent.
fn spans_for<'a>(
    syntax: &SyntaxReference,
    ps: &SyntaxSet,
    lines: impl Iterator<Item = &'a str>,
) -> Option<Vec<Vec<Token>>> {
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();

    for line in lines {
        // The parser wants the newline (some syntaxes end a context on it),
        // but it must not reach the page: the page draws the line break.
        let owned = format!("{line}\n");
        let Ok(ops) = state.parse_line(&owned, ps) else {
            // A syntax that fails mid-file would leave the rest misparsed;
            // plain text is better than half-wrong colour.
            return None;
        };
        let mut tokens: Vec<Token> = Vec::new();
        for (text, op) in ScopeRegionIterator::new(&ops, &owned) {
            if stack.apply(op).is_err() {
                return None;
            }
            let text = text.trim_end_matches('\n');
            if text.is_empty() {
                continue;
            }
            let class = classify(&stack);
            // Runs that share a class are merged, which roughly halves the
            // token count on ordinary code.
            match tokens.last_mut() {
                Some(last) if last.0 == class => last.1.push_str(text),
                _ => tokens.push(Token(class.to_string(), text.to_string())),
            }
        }
        out.push(tokens);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line's tokens must concatenate back to that exact line, or the
    /// page would render text the student never typed.
    fn assert_roundtrip(content: &str, language: &str) -> Vec<Vec<Token>> {
        let lines = highlight(content, Some(language), None).expect("highlighted");
        let expected: Vec<&str> = content.split('\n').collect();
        assert_eq!(
            lines.len(),
            expected.len(),
            "one token list per screen line"
        );
        for (tokens, line) in lines.iter().zip(expected) {
            let joined: String = tokens.iter().map(|t| t.1.as_str()).collect();
            assert_eq!(joined, line, "tokens must rebuild the line verbatim");
        }
        lines
    }

    #[test]
    fn classes_the_parts_a_reader_cares_about() {
        let lines = assert_roundtrip(
            "/* hi */\nint main(void) {\n    char *s = \"x\";\n    return 0;\n}\n",
            "c",
        );
        let class_of = |line: usize, text: &str| -> String {
            lines[line]
                .iter()
                .find(|t| t.1.contains(text))
                .unwrap_or_else(|| panic!("{text:?} on line {line}"))
                .0
                .clone()
        };
        assert_eq!(class_of(0, "hi"), "c", "block comment");
        assert_eq!(class_of(1, "int"), "k", "storage type reads as a keyword");
        assert_eq!(class_of(1, "main"), "f", "function name");
        assert_eq!(class_of(2, "x"), "s", "string body");
        assert_eq!(class_of(3, "return"), "k");
        assert_eq!(class_of(3, "0"), "n", "numeric constant");
    }

    #[test]
    fn line_count_matches_a_browser_split() {
        // A trailing newline means a final empty line on screen; Rust's
        // `lines()` would silently drop it and shift every later cursor.
        let lines = highlight("a = 1\nb = 2\n", Some("python"), None).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].is_empty());
    }

    #[test]
    fn rebuilds_unicode_and_indentation_exactly() {
        assert_roundtrip(
            "s = \"héllo — wörld\"\n\tif True:\n        pass\n",
            "python",
        );
    }

    #[test]
    fn falls_back_to_the_extension_then_to_nothing() {
        assert!(highlight("x = 1\n", None, Some("ex1/main.py")).is_some());
        assert!(highlight("x = 1\n", Some("python"), None).is_some());
        // Neither a known language nor a known extension.
        assert!(highlight("x = 1\n", Some("nonesuch"), Some("a.nonesuch")).is_none());
    }

    #[test]
    fn typescript_borrows_the_javascript_syntax() {
        let lines = highlight("const x: number = 1;\n", Some("typescript"), None).unwrap();
        assert!(lines[0].iter().any(|t| t.0 == "k" && t.1.contains("const")));
    }

    #[test]
    fn very_long_files_are_left_plain() {
        let long = "x = 1\n".repeat(MAX_LINES + 1);
        assert!(highlight(&long, Some("python"), None).is_none());
    }
}
