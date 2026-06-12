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
