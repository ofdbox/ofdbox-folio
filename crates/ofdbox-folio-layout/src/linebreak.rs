//! 断行规则。
//!
//! 输入是已测量的单元序列，输出是行区间。这里刻意不依赖字体，便于单独测试
//! CJK / 拉丁混排与行首行尾禁则。

use ofdbox_folio_ir::NoteRef;

/// 一个已测量的排版单元。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Piece {
    /// 字符；`'\n'` 表示强制断行，`'\u{0}'` 表示不可断的行内对象（图片）。
    pub ch: char,
    /// 宽度，毫米。
    pub width: f64,
    /// 样式下标。
    pub style: usize,
    /// 行内对象下标（图片）。
    pub object: Option<usize>,
    /// 单元所属的脚注/尾注引用（只标在标记的首字符上）。
    pub note: Option<NoteRef>,
}

impl Piece {
    /// 文本单元。
    pub fn text(ch: char, width: f64, style: usize) -> Self {
        Self {
            ch,
            width,
            style,
            object: None,
            note: None,
        }
    }

    /// 行内对象单元。
    pub fn object(width: f64, style: usize, object: usize) -> Self {
        Self {
            ch: '\u{0}',
            width,
            style,
            object: Some(object),
            note: None,
        }
    }

    /// 是否是强制断行。
    pub fn is_break(&self) -> bool {
        self.ch == '\n'
    }

    /// 是否是空格（不含制表符）。
    pub fn is_space(&self) -> bool {
        self.ch == ' '
    }

    /// 是否是制表符：宽度由落版时的制表位决定，测量阶段记 0。
    pub fn is_tab(&self) -> bool {
        self.ch == '\t'
    }
}

/// 一行覆盖的单元区间（左闭右开，已去掉行尾空白）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineSpan {
    /// 起始单元下标。
    pub start: usize,
    /// 结束单元下标。
    pub end: usize,
}

impl LineSpan {
    /// 单元数量。
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// 是否为空行。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 把单元切成行。
///
/// `first_width` 用于段落首行（已减去首行缩进），`width` 用于其余行。
pub(crate) fn break_lines(pieces: &[Piece], first_width: f64, width: f64) -> Vec<LineSpan> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut available = first_width.max(1.0);

    while start < pieces.len() {
        // 跳过行首的强制断行与空白（行首制表符没有意义，一并丢弃）。
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

        // 能在可断点收尾就收尾，否则按右边界硬断。
        if end < pieces.len() && !pieces[end].is_break() {
            if no_line_start(pieces[end].ch) {
                // 行首禁则：把标点吞回本行，宁可略微超宽。
                end += 1;
                while end < pieces.len() && no_line_start(pieces[end].ch) {
                    end += 1;
                }
            } else if pieces[end].is_space() {
                // 空白前可以直接断行，空白留给下一行并跳过。
            } else if let Some(candidate) = last_break
                && candidate > start
            {
                end = candidate;
            }
        }

        // 行尾禁则：不在开引号/开括号后断行。
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

/// 两个单元之间是否允许断行。
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

/// 行首禁则：这些字符不能出现在行首。
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

/// 行尾禁则：这些字符不能留在行尾。
fn no_line_end(ch: char) -> bool {
    matches!(
        ch,
        '（' | '［' | '｛' | '《' | '「' | '『' | '“' | '‘' | '(' | '[' | '{'
    )
}

/// 是否按 CJK 处理（可在字符间断行）。
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
        // "ab cd ef"，每字符 1，行宽 5：第一行 "ab cd"，第二行 "ef"。
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
        // 行尾空格不进入行区间。
        let pieces = text(&[('a', 1.0), ('b', 1.0), (' ', 1.0)]);
        let lines = break_lines(&pieces, 10.0, 10.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].end - lines[0].start, 2, "行尾空格不参与排版");
    }

    #[test]
    fn breaks_between_cjk_characters() {
        let pieces = text(&[('中', 1.0), ('文', 1.0), ('排', 1.0), ('版', 1.0)]);
        let lines = break_lines(&pieces, 2.0, 2.0);
        assert_eq!(lines.len(), 2, "{lines:?}");
    }

    #[test]
    fn keeps_closing_punctuation_with_previous_line() {
        // 行宽 2：若按宽度硬断会把「。」甩到行首，应回吞一位。
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
