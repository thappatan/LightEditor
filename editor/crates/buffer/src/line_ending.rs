//! Line-ending detection and preservation (spec §4.1.1).

/// The line-ending convention of a text buffer.
///
/// The buffer stores text verbatim — ropey keeps whatever bytes it was given.
/// This enum records the *dominant* convention so that save logic and newline
/// insertion stay consistent with how the file was loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum LineEnding {
    /// `\n` — Unix, macOS, Linux. The default for new buffers.
    #[default]
    Lf,
    /// `\r\n` — Windows.
    CrLf,
}

impl LineEnding {
    /// The string this line ending inserts.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }

    /// Detect the dominant line ending in `text`.
    ///
    /// Counts `\r\n` against bare `\n`. CRLF wins ties (if a file has any CRLF
    /// and at least as many CRLF as bare LF, it is treated as a CRLF file).
    /// Empty or newline-free text is [`LineEnding::Lf`].
    pub fn detect(text: &str) -> LineEnding {
        let crlf = text.matches("\r\n").count();
        let total_lf = text.bytes().filter(|&b| b == b'\n').count();
        let bare_lf = total_lf - crlf;
        if crlf > 0 && crlf >= bare_lf {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_round_trips() {
        assert_eq!(LineEnding::Lf.as_str(), "\n");
        assert_eq!(LineEnding::CrLf.as_str(), "\r\n");
    }

    #[test]
    fn default_is_lf() {
        assert_eq!(LineEnding::default(), LineEnding::Lf);
    }

    #[test]
    fn detect_empty_and_newline_free_is_lf() {
        assert_eq!(LineEnding::detect(""), LineEnding::Lf);
        assert_eq!(LineEnding::detect("no newline here"), LineEnding::Lf);
    }

    #[test]
    fn detect_pure_lf() {
        assert_eq!(LineEnding::detect("a\nb\nc\n"), LineEnding::Lf);
    }

    #[test]
    fn detect_pure_crlf() {
        assert_eq!(LineEnding::detect("a\r\nb\r\nc\r\n"), LineEnding::CrLf);
    }

    #[test]
    fn detect_mixed_prefers_majority() {
        // 2 CRLF, 1 bare LF → CRLF
        assert_eq!(LineEnding::detect("a\r\nb\r\nc\nd"), LineEnding::CrLf);
        // 1 CRLF, 2 bare LF → LF
        assert_eq!(LineEnding::detect("a\r\nb\nc\nd"), LineEnding::Lf);
    }

    #[test]
    fn detect_tie_goes_to_crlf() {
        // 1 CRLF, 1 bare LF → CRLF wins the tie
        assert_eq!(LineEnding::detect("a\r\nb\nc"), LineEnding::CrLf);
    }
}
