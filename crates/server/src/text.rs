//! Plain-text extraction from raw terminal bytes.

/// Strips ANSI escape sequences from raw terminal bytes and returns a
/// lossy-UTF8 string. This is the readable/searchable form used for offline
/// analysis; the verbatim bytes are kept separately for faithful replay.
///
/// Note: this removes escape sequences but does not emulate a terminal, so
/// in-place cursor movement (progress bars, full-screen apps) won't be folded
/// into final on-screen text. For shell command transcripts it is accurate.
pub fn plain(data: &[u8]) -> String {
    let stripped = strip_ansi_escapes::strip(data);
    String::from_utf8_lossy(&stripped).into_owned()
}

/// Strong indicators that a chunk of terminal output represents an error —
/// used to flag students who may be stuck. Deliberately conservative (e.g. not
/// the bare word "error") to avoid false positives.
const ERROR_SIGNALS: &[&str] = &[
    "traceback (most recent call last)",
    "error:",
    "exception",
    "panicked at",
    "command not found",
    "no such file or directory",
    "segmentation fault",
    "assertionerror",
    "fatal:",
    "syntaxerror",
];

/// Whether a piece of (already ANSI-stripped) output looks like an error.
pub fn looks_like_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ERROR_SIGNALS.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::looks_like_error;

    #[test]
    fn detects_errors() {
        assert!(looks_like_error("Traceback (most recent call last):"));
        assert!(looks_like_error(
            "error: cannot find value `x` in this scope"
        ));
        assert!(looks_like_error("thread 'main' panicked at src/main.rs:4"));
        assert!(looks_like_error("bash: foo: command not found"));
    }

    #[test]
    fn ignores_normal_output() {
        assert!(!looks_like_error("all tests passed"));
        assert!(!looks_like_error("Compiling hermione v0.1.0"));
        assert!(!looks_like_error("0 errors, 0 warnings"));
    }
}
