//! Line breaking rules.
//!
//! The input is a sequence of already measured pieces and the output is line
//! ranges. This deliberately does not depend on fonts so that CJK / Latin mixed
//! text and the line-start / line-end prohibition rules can be tested in
//! isolation.

use ofdbox_folio_ir::NoteRef;

/// A piece that has already been measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Piece {
    /// Character; `'\n'` means a forced break, `'\u{0}'` means an unbreakable
    /// inline object (image).
    pub ch: char,
    /// Width in millimeters.
    pub width: f64,
    /// Style index.
    pub style: usize,
    /// Inline object index (image).
    pub object: Option<usize>,
    /// The footnote / endnote reference this piece belongs to (marked only on
    /// the first character of the marker).
    pub note: Option<NoteRef>,
}

impl Piece {
    /// Text piece.
    pub fn text(ch: char, width: f64, style: usize) -> Self {
        Self {
            ch,
            width,
            style,
            object: None,
            note: None,
        }
    }

    /// Inline object piece.
    pub fn object(width: f64, style: usize, object: usize) -> Self {
        Self {
            ch: '\u{0}',
            width,
            style,
            object: Some(object),
            note: None,
        }
    }

    /// Whether this is a forced break.
    pub fn is_break(&self) -> bool {
        self.ch == '\n'
    }

    /// Whether this is a space (tabs excluded).
    pub fn is_space(&self) -> bool {
        self.ch == ' '
    }

    /// Whether this is a tab: the width is determined by the tab stops at
    /// layout time, so it is recorded as 0 during measurement.
    pub fn is_tab(&self) -> bool {
        self.ch == '\t'
    }
}

/// The piece range covered by one line (left-closed, right-open; trailing
/// whitespace already removed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineSpan {
    /// Start piece index.
    pub start: usize,
    /// End piece index.
    pub end: usize,
}

impl LineSpan {
    /// Number of pieces.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether this is an empty line.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Cut pieces into lines.
///
/// `first_width` is used for the first line of a paragraph (already reduced by
/// the first-line indent) and `width` for the remaining lines.
pub(crate) fn break_lines(pieces: &[Piece], first_width: f64, width: f64) -> Vec<LineSpan> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut available = first_width.max(1.0);

    while start < pieces.len() {
        // Skip forced breaks and whitespace at the line start (a tab at the
        // line start is meaningless, so it is dropped as well).
        while start < pieces.len()
            && (pieces[start].is_break() || pieces[start].is_space() || pieces[start].is_tab())
        {
            start += 1;
        }
        if start >= pieces.len() {
            break;
        }

        let mut end = start;
        let mut used = 0.0;
        let mut last_break: Option<usize> = None;

        while end < pieces.len() {
            let piece = pieces[end];
            if piece.is_break() {
                break;
            }
            if end > start && used + piece.width > available + 1e-9 {
                break;
            }
            used += piece.width;
            end += 1;
            if can_break_between(pieces, end) {
                last_break = Some(end);
            }
        }

        // End the line at a break opportunity when possible, otherwise break
        // hard at the right boundary.
        if end < pieces.len() && !pieces[end].is_break() {
            if no_line_start(pieces[end].ch) {
                // Line-start prohibition: pull the punctuation back into this
                // line, accepting a slight overflow.
                end += 1;
                while end < pieces.len() && no_line_start(pieces[end].ch) {
                    end += 1;
                }
            } else if pieces[end].is_space() {
                // We can break directly before whitespace; the whitespace is
                // left for the next line and skipped.
            } else if let Some(candidate) = last_break
                && candidate > start
            {
                end = candidate;
            }
        }

        // Line-end prohibition: do not break after an opening quote / opening
        // bracket.
        while end > start + 1 && end < pieces.len() && no_line_end(pieces[end - 1].ch) {
            end += 1;
        }

        let mut trimmed = end;
        while trimmed > start && (pieces[trimmed - 1].is_space() || pieces[trimmed - 1].is_tab()) {
            trimmed -= 1;
        }
        lines.push(LineSpan {
            start,
            end: trimmed,
        });

        start = end;
        available = width.max(1.0);
    }

    lines
}

/// Whether a break is allowed between two pieces.
fn can_break_between(pieces: &[Piece], index: usize) -> bool {
    if index == 0 || index >= pieces.len() {
        return false;
    }
    let prev = pieces[index - 1];
    let next = pieces[index];
    if prev.is_space() || prev.is_tab() || prev.ch == '-' || prev.ch == '\u{2014}' {
        return true;
    }
    if prev.object.is_some() || next.object.is_some() {
        return true;
    }
    if is_cjk(prev.ch) || is_cjk(next.ch) {
        return !no_line_start(next.ch) && !no_line_end(prev.ch);
    }
    false
}

/// Line-start prohibition: these characters may not appear at the line start.
fn no_line_start(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '）'
            | '］'
            | '｝'
            | '》'
            | '」'
            | '』'
            | '”'
            | '’'
            | '%'
            | '‰'
            | '℃'
            | '·'
            | '…'
            | ','
            | '.'
            | ';'
            | ':'
            | '?'
            | '!'
            | ')'
            | ']'
            | '}'
    )
}

/// Line-end prohibition: these characters may not stay at the line end.
fn no_line_end(ch: char) -> bool {
    matches!(
        ch,
        '（' | '［' | '｛' | '《' | '「' | '『' | '“' | '‘' | '(' | '[' | '{'
    )
}

/// Whether the character is treated as CJK (breakable between characters).
fn is_cjk(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x2E80..=0x2FFF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FA1F
    )
}

#[cfg(test)]
mod tests {
    use super::{Piece, break_lines};

    fn text(spec: &[(char, f64)]) -> Vec<Piece> {
        spec.iter()
            .map(|(ch, width)| Piece::text(*ch, *width, 0))
            .collect()
    }

    #[test]
    fn breaks_latin_on_spaces() {
        // "ab cd ef", 1 per character, line width 5: first line "ab cd", second
        // line "ef".
        let pieces = text(&[
            ('a', 1.0),
            ('b', 1.0),
            (' ', 1.0),
            ('c', 1.0),
            ('d', 1.0),
            (' ', 1.0),
            ('e', 1.0),
            ('f', 1.0),
        ]);
        let lines = break_lines(&pieces, 5.0, 5.0);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].end - lines[0].start, 5);
        assert_eq!(&pieces[lines[1].start..lines[1].end].len(), &2);
    }

    #[test]
    fn trims_trailing_spaces() {
        // Trailing spaces do not enter the line range.
        let pieces = text(&[('a', 1.0), ('b', 1.0), (' ', 1.0)]);
        let lines = break_lines(&pieces, 10.0, 10.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].end - lines[0].start,
            2,
            "trailing spaces are not typeset"
        );
    }

    #[test]
    fn breaks_between_cjk_characters() {
        let pieces = text(&[('中', 1.0), ('文', 1.0), ('排', 1.0), ('版', 1.0)]);
        let lines = break_lines(&pieces, 2.0, 2.0);
        assert_eq!(lines.len(), 2, "{lines:?}");
    }

    #[test]
    fn keeps_closing_punctuation_with_previous_line() {
        // Line width 2: a hard break by width would push "。" to the line
        // start, so one piece is taken back.
        let pieces = text(&[('中', 1.0), ('文', 1.0), ('。', 1.0)]);
        let lines = break_lines(&pieces, 2.0, 2.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].end - lines[0].start, 3);
    }

    #[test]
    fn honours_explicit_line_breaks() {
        let pieces = text(&[('a', 1.0), ('\n', 0.0), ('b', 1.0)]);
        let lines = break_lines(&pieces, 10.0, 10.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 1);
    }

    #[test]
    fn places_oversized_piece_alone() {
        let pieces = text(&[('a', 1.0), ('b', 20.0), ('c', 1.0)]);
        let lines = break_lines(&pieces, 5.0, 5.0);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[1].len(), 1);
    }
}
