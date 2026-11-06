//! Markdown to Flow IR adapter for OFDBox Folio.
//!
//! The parser targets the common Markdown used by READMEs and notes and maps it to
//! Flow IR, so a Markdown file can be laid out and exported through the same
//! engine as DOCX:
//!
//! - ATX headings (`#` … `######`) become outline headings;
//! - paragraphs with emphasis, strong text, strikethrough, inline code, links and
//!   inline images;
//! - ordered / unordered lists, block quotes and fenced code blocks;
//! - pipe tables and standalone images;
//! - horizontal rules are rendered as a light divider.
//!
//! Parsing is deliberately tolerant: unsupported syntax is reported as a
//! diagnostic on the returned [`FlowDocument`] instead of failing the whole file.
//!
//! ```text
//! let flow = ofdbox_folio_markdown::read_markdown(reader, &MarkdownOptions::default());
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, AssetId, Block, Border, BorderStyle, Cell, Column, ColumnWidth, Diagnostic,
    DiagnosticCode, FlowDocument, FontFamily, ImageAsset, ImageBlock, ImageFormat, Inline,
    InlineImage, LineSpacing, Link, LinkTarget, Margins, Metadata, PageSetup, ParaStyle, Paragraph,
    Rgba, Row, Severity, Table, TableBorders, TableWidth, TextRun, TextStyle, Twips,
    detect_image_format, image_dimensions,
};

/// Error returned when a Markdown file cannot be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MarkdownError {
    /// File system error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not valid UTF-8.
    #[error("invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Markdown parsing options.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownOptions {
    /// Body text style.
    pub body: TextStyle,
    /// Inline code and fenced code style.
    pub code: TextStyle,
    /// Page setup.
    pub page_setup: PageSetup,
    /// Base directory for relative image paths.
    pub base_dir: Option<PathBuf>,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        let body = TextStyle {
            family: FontFamily::new("SimSun"),
            size: Twips::from_pt(11.0),
            ..TextStyle::default()
        };
        let code = TextStyle {
            family: FontFamily::new("Courier New"),
            size: Twips::from_pt(9.5),
            color: Rgba::from_rgb(0x2B, 0x2B, 0x2B),
            ..TextStyle::default()
        };
        Self {
            body,
            code,
            page_setup: PageSetup::a4(),
            base_dir: None,
        }
    }
}

impl MarkdownOptions {
    /// Use a base directory for relative image references.
    pub fn with_base_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.base_dir = Some(dir.into());
        self
    }
}

/// Parse Markdown from a string.
pub fn read_markdown(text: &str, options: &MarkdownOptions) -> FlowDocument {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    Parser::new(&normalized, options).parse()
}

/// Parse Markdown from UTF-8 bytes.
pub fn read_markdown_bytes(
    bytes: &[u8],
    options: &MarkdownOptions,
) -> Result<FlowDocument, MarkdownError> {
    let text = std::str::from_utf8(bytes)?;
    Ok(read_markdown(text, options))
}

/// Read and parse a Markdown file.
///
/// Relative image paths are resolved against the file's parent directory unless
/// [`MarkdownOptions::base_dir`] is already set.
pub fn read_markdown_path(
    path: impl AsRef<Path>,
    mut options: MarkdownOptions,
) -> Result<FlowDocument, MarkdownError> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes)?;
    if options.base_dir.is_none() {
        options.base_dir = path.parent().map(Path::to_path_buf);
    }
    Ok(read_markdown(text, &options))
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    index: usize,
    options: &'a MarkdownOptions,
    document: FlowDocument,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str, options: &'a MarkdownOptions) -> Self {
        let mut document = FlowDocument::new(Metadata::default());
        document.sections[0].setup = options.page_setup;
        document.styles.default_character = options.body.clone();
        Self {
            lines: text.lines().collect(),
            index: 0,
            options,
            document,
        }
    }

    fn parse(mut self) -> FlowDocument {
        while self.index < self.lines.len() {
            if self.lines[self.index].trim().is_empty() {
                self.index += 1;
                continue;
            }
            let line = self.lines[self.index];
            if let Some((fence, info)) = fence_start(line) {
                let indent = line.len() - line.trim_start().len();
                self.parse_code_block(fence, info, indent);
            } else if let Some((level, heading)) = heading(line) {
                self.parse_heading(level, heading);
            } else if is_horizontal_rule(line) {
                self.parse_horizontal_rule();
            } else if line.trim_start().starts_with('>') {
                self.parse_blockquote();
            } else if is_table_start(&self.lines, self.index) {
                self.parse_table();
            } else if list_marker(line).is_some() {
                self.parse_list_item();
            } else if let Some((alt, src)) = standalone_image(line) {
                self.parse_block_image(alt, src);
            } else if line.trim_start().starts_with('<') {
                self.diagnostic(DiagnosticCode::Unsupported, "inline HTML is not rendered");
                self.index += 1;
            } else {
                self.parse_paragraph();
            }
        }
        self.document
    }

    fn parse_heading(&mut self, level: u8, text: &str) {
        let size = match level {
            1 => 24.0,
            2 => 20.0,
            3 => 16.0,
            4 => 14.0,
            5 => 12.0,
            _ => 11.0,
        };
        let style = TextStyle {
            size: Twips::from_pt(size),
            bold: true,
            ..self.options.body.clone()
        };
        let mut paragraph = Paragraph::plain("", style.clone());
        paragraph.heading_level = Some(level);
        paragraph.style.space_before = Twips::from_pt(if level <= 2 { 12.0 } else { 8.0 });
        paragraph.style.space_after = Twips::from_pt(6.0);
        paragraph.style.keep_with_next = true;
        paragraph.runs = parse_inlines(text, &style, &mut self.document, self.options);
        if self.document.metadata.title.is_none() {
            self.document.metadata.title = Some(plain_text(&paragraph.runs));
        }
        self.document.push(Block::Paragraph(paragraph));
        self.index += 1;
    }

    fn parse_paragraph(&mut self) {
        let mut text = String::new();
        while self.index < self.lines.len() {
            let line = self.lines[self.index];
            if line.trim().is_empty() || starts_block(&self.lines, self.index) {
                break;
            }
            let trimmed = line.trim();
            let hard_break = trimmed.ends_with("  ") || trimmed.ends_with('\\');
            let trimmed = trimmed.trim_end_matches('\\').trim_end();
            if !text.is_empty() {
                if hard_break {
                    text.push('\n');
                } else if needs_space(&text, trimmed) {
                    text.push(' ');
                }
            }
            text.push_str(trimmed);
            self.index += 1;
        }
        if text.is_empty() {
            return;
        }
        let mut paragraph = Paragraph::new(body_paragraph_style());
        paragraph.runs = parse_inlines(
            &text,
            &self.options.body.clone(),
            &mut self.document,
            self.options,
        );
        self.document.push(Block::Paragraph(paragraph));
    }

    fn parse_code_block(&mut self, fence: &str, _info: &str, fence_indent: usize) {
        self.index += 1;
        let mut lines = Vec::new();
        while self.index < self.lines.len() {
            let line = self.lines[self.index];
            if line.trim_start().starts_with(fence) {
                self.index += 1;
                break;
            }
            // Strip the opening fence's own indentation, then keep everything else: leading
            // whitespace is meaningful inside a code block.
            let mut content = line;
            let mut removed = 0;
            while removed < fence_indent && content.starts_with(' ') {
                content = &content[1..];
                removed += 1;
            }
            lines.push(code_line(content));
            self.index += 1;
        }
        // Every source line becomes its own paragraph so that blank lines inside the block keep
        // their height (the layout collapses consecutive forced breaks in a single run).
        let mut blocks = Vec::with_capacity(lines.len().max(1));
        for line in &lines {
            let mut style = body_paragraph_style();
            style.line_spacing = LineSpacing::Single;
            style.space_before = Twips::default();
            style.space_after = Twips::default();
            style.left_indent = Twips::default();
            style.right_indent = Twips::default();
            let mut paragraph = Paragraph::new(style);
            if !line.is_empty() {
                paragraph.runs.push(Inline::Text(TextRun::new(
                    line.clone(),
                    self.options.code.clone(),
                )));
            }
            blocks.push(Block::Paragraph(paragraph));
        }
        if blocks.is_empty() {
            blocks.push(Block::Paragraph(Paragraph::new(body_paragraph_style())));
        }
        // A one-cell shaded table gives the block a background and padding; the table also
        // provides the gap to the surrounding paragraphs.
        let mut table = Table::new(vec![Column {
            width: ColumnWidth::Weight(1),
        }]);
        table.width = TableWidth::Percent(100.0);
        table.cell_padding = Margins::uniform(Twips::from_mm(2.0));
        let mut cell = Cell::new(blocks);
        cell.shading = Some(code_block_background());
        cell.padding = Some(Margins {
            top: Twips::from_mm(2.0),
            right: Twips::from_mm(2.5),
            bottom: Twips::from_mm(2.5),
            left: Twips::from_mm(2.5),
        });
        table.rows.push(Row::new(vec![cell]));
        self.document.push(Block::Table(table));
        // Tables carry no block spacing, so a short empty paragraph keeps a gap between the
        // shaded block and the following text.
        let mut spacer = body_paragraph_style();
        spacer.space_before = Twips::default();
        spacer.space_after = Twips::default();
        spacer.line_spacing = LineSpacing::Exact(Twips::from_pt(6.0));
        self.document.push(Block::Paragraph(Paragraph::new(spacer)));
    }

    fn parse_blockquote(&mut self) {
        let mut text = String::new();
        while self.index < self.lines.len() {
            let line = self.lines[self.index];
            let Some(rest) = line.trim_start().strip_prefix('>') else {
                break;
            };
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(rest.trim_end());
            self.index += 1;
        }
        let mut style = body_paragraph_style();
        style.left_indent = Twips::from_mm(6.0);
        style.right_indent = Twips::from_mm(2.0);
        let mut paragraph = Paragraph::new(style);
        paragraph
            .runs
            .push(Inline::Text(TextRun::new("▏ ", self.options.body.clone())));
        paragraph.runs.extend(parse_inlines(
            &text,
            &self.options.body.clone(),
            &mut self.document,
            self.options,
        ));
        self.document.push(Block::Paragraph(paragraph));
    }

    fn parse_list_item(&mut self) {
        let Some(marker) = list_marker(self.lines[self.index]) else {
            self.index += 1;
            return;
        };
        let mut text = marker.content.to_owned();
        let base_indent = marker.indent;
        self.index += 1;
        while self.index < self.lines.len() {
            let line = self.lines[self.index];
            if line.trim().is_empty() {
                break;
            }
            let indent = leading_indent(line);
            if list_marker(line).is_some() || indent <= base_indent {
                break;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(line.trim());
            self.index += 1;
        }
        let mut style = body_paragraph_style();
        let depth = base_indent / 2;
        style.left_indent = Twips::from_mm(6.0 + depth as f64 * 6.0);
        style.first_line_indent = Twips::from_mm(-4.0);
        style.space_after = Twips::from_pt(2.0);
        let mut paragraph = Paragraph::new(style);
        paragraph.runs.push(Inline::Text(TextRun::new(
            format!("{} ", marker.marker),
            self.options.body.clone(),
        )));
        paragraph.runs.extend(parse_inlines(
            &text,
            &self.options.body.clone(),
            &mut self.document,
            self.options,
        ));
        self.document.push(Block::Paragraph(paragraph));
    }

    fn parse_block_image(&mut self, alt: &str, src: &str) {
        self.index += 1;
        if let Some(id) = load_image(src, &mut self.document, self.options) {
            let mut image = ImageBlock::new(id);
            image.align = Align::Center;
            image.alt = (!alt.is_empty()).then(|| alt.to_owned());
            self.document.push(Block::Image(image));
        }
    }

    fn parse_horizontal_rule(&mut self) {
        self.index += 1;
        let mut style = body_paragraph_style();
        style.align = Align::Center;
        style.space_before = Twips::from_pt(6.0);
        style.space_after = Twips::from_pt(6.0);
        let mut paragraph = Paragraph::new(style);
        paragraph.runs.push(Inline::Text(TextRun::new(
            "────────────────────────",
            TextStyle {
                size: Twips::from_pt(8.0),
                color: Rgba::from_rgb(0x99, 0x99, 0x99),
                ..self.options.body.clone()
            },
        )));
        self.document.push(Block::Paragraph(paragraph));
    }

    fn parse_table(&mut self) {
        let header = split_table_row(self.lines[self.index]);
        self.index += 2; // header + separator
        let mut rows: Vec<Vec<String>> = vec![header];
        while self.index < self.lines.len() {
            let line = self.lines[self.index];
            if line.trim().is_empty() || !line.contains('|') {
                break;
            }
            rows.push(split_table_row(line));
            self.index += 1;
        }
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut table = Table::new(
            (0..columns)
                .map(|_| Column {
                    width: ColumnWidth::Weight(1),
                })
                .collect(),
        );
        table.cell_padding = Margins::uniform(Twips::from_mm(1.5));
        table.borders = TableBorders {
            outer: Some(light_border()),
            horizontal: Some(light_border()),
            vertical: Some(light_border()),
        };
        for (row_index, cells) in rows.into_iter().enumerate() {
            let mut row = Row::new(
                (0..columns)
                    .map(|index| {
                        let text = cells.get(index).map(String::as_str).unwrap_or("");
                        table_cell(text, self.options)
                    })
                    .collect(),
            );
            row.is_header = row_index == 0;
            table.rows.push(row);
        }
        self.document.push(Block::Table(table));
    }

    fn diagnostic(&mut self, code: DiagnosticCode, message: &str) {
        self.document
            .report
            .push(Diagnostic::new(Severity::Warning, code, message.to_owned()));
    }
}

/// Prepares one code line for the Flow IR.
///
/// The line breaker trims whitespace at the start of a line, so leading spaces and tabs are
/// encoded as non-breaking spaces; tabs are expanded (four columns) because the layout has no
/// tab stops for code blocks.
fn code_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut leading = true;
    for ch in line.chars() {
        match ch {
            '\t' if leading => out.push_str("\u{00A0}\u{00A0}\u{00A0}\u{00A0}"),
            '\t' => out.push_str("    "),
            ' ' if leading => out.push('\u{00A0}'),
            ' ' => out.push(' '),
            _ => {
                leading = false;
                out.push(ch);
            }
        }
    }
    out
}

fn code_block_background() -> Rgba {
    Rgba::from_rgb(0xF4, 0xF4, 0xF4)
}

fn body_paragraph_style() -> ParaStyle {
    ParaStyle {
        line_spacing: LineSpacing::Multiple(1.25),
        space_after: Twips::from_pt(6.0),
        ..ParaStyle::default()
    }
}

fn table_cell(text: &str, options: &MarkdownOptions) -> Cell {
    let mut style = body_paragraph_style();
    style.line_spacing = LineSpacing::Single;
    style.space_after = Twips::from_pt(0.0);
    let mut paragraph = Paragraph::new(style);
    let mut scratch = FlowDocument::new(Metadata::default());
    paragraph.runs = parse_inlines(text.trim(), &options.body.clone(), &mut scratch, options);
    Cell::new(vec![Block::Paragraph(paragraph)])
}

fn light_border() -> Border {
    Border {
        width: Twips::from_mm(0.2),
        style: BorderStyle::Solid,
        color: Rgba::from_rgb(0xCC, 0xCC, 0xCC),
    }
}

fn plain_text(runs: &[Inline]) -> String {
    let mut text = String::new();
    for inline in runs {
        match inline {
            Inline::Text(run) => text.push_str(&run.text),
            Inline::Link(link) => text.push_str(&plain_text(&link.runs)),
            _ => {}
        }
    }
    text
}

fn needs_space(previous: &str, next: &str) -> bool {
    let Some(last) = previous.chars().last() else {
        return false;
    };
    let Some(first) = next.chars().next() else {
        return false;
    };
    !is_cjk(last) && !is_cjk(first)
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x9FFF | 0xF900..=0xFAFF
    )
}

fn leading_indent(line: &str) -> usize {
    line.chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

fn fence_start(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    for marker in ["```", "~~~"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some((marker, rest.trim()));
        }
    }
    None
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed[hashes..].strip_prefix(' ')?;
    Some((hashes as u8, rest.trim_end_matches('#').trim_end()))
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    trimmed.chars().all(|ch| ch == first || ch == ' ')
}

#[derive(Clone, Copy)]
struct ListMarker<'a> {
    indent: usize,
    marker: &'a str,
    content: &'a str,
}

fn list_marker(line: &str) -> Option<ListMarker<'_>> {
    let indent = leading_indent(line);
    let trimmed = line.trim_start();
    let (marker, content) = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        ("•", rest)
    } else {
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let after = trimmed.get(digits..)?;
        let rest = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))?;
        (&trimmed[..digits], rest)
    };
    Some(ListMarker {
        indent,
        marker,
        content,
    })
}

fn starts_block(lines: &[&str], index: usize) -> bool {
    let line = lines[index];
    fence_start(line).is_some()
        || heading(line).is_some()
        || is_horizontal_rule(line)
        || line.trim_start().starts_with('>')
        || list_marker(line).is_some()
        || standalone_image(line).is_some()
        || is_table_start(lines, index)
}

fn standalone_image(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if !trimmed.starts_with("![") || !trimmed.ends_with(')') {
        return None;
    }
    parse_link_like(trimmed, 1)
}

fn is_table_start(lines: &[&str], index: usize) -> bool {
    if index + 1 >= lines.len() || !lines[index].contains('|') {
        return false;
    }
    let separator = split_table_row(lines[index + 1]);
    !separator.is_empty()
        && separator.iter().all(|cell| {
            !cell.trim().is_empty() && cell.trim().chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

fn split_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_matches('|');
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn parse_link_like(text: &str, skip: usize) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut index = skip;
    let mut depth = 0_usize;
    let label_start = skip + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let label = text.get(label_start..index)?;
                    let rest = text.get(index + 1..)?;
                    let rest = rest.strip_prefix('(')?.strip_suffix(')')?;
                    let target = rest.split_whitespace().next().unwrap_or(rest);
                    return Some((label, target));
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn parse_inlines(
    text: &str,
    style: &TextStyle,
    document: &mut FlowDocument,
    options: &MarkdownOptions,
) -> Vec<Inline> {
    let chars: Vec<char> = text.chars().collect();
    parse_inline_range(&chars, 0, chars.len(), style, document, options)
}

fn parse_inline_range(
    chars: &[char],
    start: usize,
    end: usize,
    style: &TextStyle,
    document: &mut FlowDocument,
    options: &MarkdownOptions,
) -> Vec<Inline> {
    let mut runs = Vec::new();
    let mut buffer = String::new();
    let mut index = start;
    while index < end {
        let ch = chars[index];
        if ch == '\\' && index + 1 < end {
            buffer.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if chars[index] == '`' {
            let ticks = count_run(chars, index, end, '`');
            if let Some(close) = find_run(chars, index + ticks, end, '`', ticks) {
                let content: String = chars[index + ticks..close].iter().collect();
                push_text(&mut runs, &mut buffer, style);
                let mut code_style = options.code.clone();
                code_style.size = style.size;
                runs.push(Inline::Text(TextRun::new(content.trim(), code_style)));
                index = close + ticks;
                continue;
            }
        }
        if chars[index] == '!'
            && chars.get(index + 1) == Some(&'[')
            && let Some((label, target, consumed)) = parse_inline_link(chars, index + 1, end)
        {
            push_text(&mut runs, &mut buffer, style);
            if let Some(id) = load_image(&target, document, options) {
                let mut image = InlineImage::new(id);
                image.alt = (!label.is_empty()).then_some(label);
                runs.push(Inline::Image(image));
            } else {
                runs.push(Inline::Text(TextRun::new(label, style.clone())));
            }
            index = consumed;
            continue;
        }
        if ch == '['
            && let Some((label, target, consumed)) = parse_inline_link(chars, index, end)
        {
            push_text(&mut runs, &mut buffer, style);
            let mut link_style = style.clone();
            link_style.underline = true;
            link_style.color = Rgba::from_rgb(0x1A, 0x5F, 0xB4);
            let children = parse_inline_range(
                chars,
                index + 1,
                index + 1 + label.chars().count(),
                &link_style,
                document,
                options,
            );
            runs.push(Inline::Link(Link::new(LinkTarget::Uri(target), children)));
            index = consumed;
            continue;
        }
        if ch == '<'
            && let Some(close) = (index + 1..end).find(|index| chars[*index] == '>')
        {
            let target: String = chars[index + 1..close].iter().collect();
            if target.starts_with("http://") || target.starts_with("https://") {
                push_text(&mut runs, &mut buffer, style);
                runs.push(Inline::Link(Link::new(
                    LinkTarget::Uri(target.clone()),
                    vec![Inline::Text(TextRun::new(target, style.clone()))],
                )));
                index = close + 1;
                continue;
            }
        }
        if matches!(ch, '*' | '_' | '~') {
            let marker_len = count_run(chars, index, end, ch);
            if (marker_len >= 2 || ch != '~')
                && let Some(close) = find_run(chars, index + marker_len, end, ch, marker_len)
            {
                push_text(&mut runs, &mut buffer, style);
                let mut inner = style.clone();
                if marker_len >= 2 {
                    if ch == '~' {
                        inner.strike = true;
                    } else {
                        inner.bold = true;
                    }
                } else {
                    inner.italic = true;
                }
                runs.extend(parse_inline_range(
                    chars,
                    index + marker_len,
                    close,
                    &inner,
                    document,
                    options,
                ));
                index = close + marker_len;
                continue;
            }
        }
        buffer.push(ch);
        index += 1;
    }
    push_text(&mut runs, &mut buffer, style);
    runs
}

fn push_text(runs: &mut Vec<Inline>, buffer: &mut String, style: &TextStyle) {
    if buffer.is_empty() {
        return;
    }
    runs.push(Inline::Text(TextRun::new(
        std::mem::take(buffer),
        style.clone(),
    )));
}

fn count_run(chars: &[char], start: usize, end: usize, marker: char) -> usize {
    let mut count = 0;
    let mut index = start;
    while index < end && chars[index] == marker {
        count += 1;
        index += 1;
    }
    count
}

fn find_run(chars: &[char], start: usize, end: usize, marker: char, len: usize) -> Option<usize> {
    let mut index = start;
    while index + len <= end {
        if chars[index] == marker
            && (0..len).all(|offset| chars.get(index + offset) == Some(&marker))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Parses `[label](target)` starting at `start` and returns the label, target and
/// the index just after the closing parenthesis.
fn parse_inline_link(chars: &[char], start: usize, end: usize) -> Option<(String, String, usize)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let mut depth = 0_usize;
    let mut close = None;
    for (offset, ch) in chars[start..end].iter().enumerate() {
        let index = start + offset;
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let target_end = (close + 2..end).find(|index| chars[*index] == ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    let target: String = chars[close + 2..target_end].iter().collect();
    let target = target.split_whitespace().next().unwrap_or("").to_owned();
    Some((label, target, target_end + 1))
}

fn load_image(
    src: &str,
    document: &mut FlowDocument,
    options: &MarkdownOptions,
) -> Option<AssetId> {
    if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("data:") {
        document.report.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::Image,
            format!("remote image is not fetched: {src}"),
        ));
        return None;
    }
    let path = match &options.base_dir {
        Some(dir) => dir.join(src),
        None => PathBuf::from(src),
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            document.report.push(Diagnostic::new(
                Severity::Warning,
                DiagnosticCode::Image,
                format!("image could not be read {}: {error}", path.display()),
            ));
            return None;
        }
    };
    let Some(format) = detect_image_format(&bytes) else {
        document.report.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::Image,
            format!("unsupported image format: {}", path.display()),
        ));
        return None;
    };
    let Some((width, height)) = image_dimensions(&bytes) else {
        document.report.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::Image,
            format!("image dimensions are unknown: {}", path.display()),
        ));
        return None;
    };
    let asset = ImageAsset::new(format, Arc::from(bytes.as_slice()), width, height);
    Some(document.resources.add_image(asset))
}

/// Write a Flow document as Markdown.
///
/// Inline emphasis, links, tables and images are preserved. Embedded images are
/// written as data URIs, and notes are written as `[^id]` footnotes. Page layout,
/// colors, headers and footers have no Markdown representation and are omitted.
pub fn write_markdown(document: &FlowDocument) -> String {
    let mut out = String::new();
    for section in &document.sections {
        for block in &section.blocks {
            write_block(block, document, &mut out);
        }
    }
    for note in &document.notes {
        let text = note_text(&note.blocks, document);
        if !text.is_empty() {
            out.push_str(&format!("[^{}]: {}\n", note.id, text));
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn write_block(block: &Block, document: &FlowDocument, out: &mut String) {
    match block {
        Block::Paragraph(paragraph) => {
            let text = paragraph_markdown(paragraph, document);
            if text.trim().is_empty() {
                return;
            }
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(text.trim_end());
            out.push_str("\n\n");
        }
        Block::Table(table) => {
            if let Some(code) = code_block_text(table) {
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                out.push_str("```\n");
                out.push_str(&code);
                out.push_str("\n```\n\n");
                return;
            }
            write_table(table, document, out);
            out.push('\n');
        }
        Block::Image(image) => {
            if let Some(url) = image_data_uri(document, image.image) {
                let alt = image.alt.as_deref().unwrap_or("");
                out.push_str(&format!("![{alt}]({url})\n\n"));
            }
        }
        Block::Toc(_) => out.push_str("[TOC]\n\n"),
        Block::PageBreak => out.push_str("---\n\n"),
        Block::Floating(floating) => write_block(&floating.block, document, out),
        _ => {}
    }
}

fn paragraph_markdown(paragraph: &Paragraph, document: &FlowDocument) -> String {
    let content = paragraph
        .runs
        .iter()
        .map(|inline| inline_markdown(inline, document))
        .collect::<String>();
    if let Some(level) = paragraph.heading_level {
        let title = paragraph.runs.iter().map(plain_inline).collect::<String>();
        return format!("{} {}", "#".repeat(usize::from(level)), title.trim());
    }
    if content.trim().chars().all(|ch| ch == '─') {
        return "---".to_owned();
    }
    if let Some(rest) = content.strip_prefix("▏ ") {
        return format!("> {rest}");
    }
    if let Some(rest) = content
        .strip_prefix("• ")
        .or_else(|| content.strip_prefix("· "))
    {
        let indent = list_indent(paragraph.style.left_indent);
        return format!("{indent}- {}", rest.trim());
    }
    let indent = list_indent(paragraph.style.left_indent);
    if indent.is_empty() {
        content
    } else {
        format!("{indent}{content}")
    }
}

fn list_indent(left: Twips) -> String {
    let millimetres = left.to_mm().max(0.0);
    let level = (millimetres / 6.0).floor() as usize;
    "  ".repeat(level)
}

/// Recognises the one-cell shaded table produced for fenced code blocks.
fn code_block_text(table: &Table) -> Option<String> {
    let [row] = table.rows.as_slice() else {
        return None;
    };
    let [cell] = row.cells.as_slice() else {
        return None;
    };
    if cell.shading != Some(code_block_background()) {
        return None;
    }
    let mut lines = Vec::with_capacity(cell.blocks.len());
    for block in &cell.blocks {
        let Block::Paragraph(paragraph) = block else {
            return None;
        };
        let mut line = String::new();
        for inline in &paragraph.runs {
            let Inline::Text(run) = inline else {
                return None;
            };
            if !is_code_font(&run.style) {
                return None;
            }
            line.push_str(&run.text.replace('\u{00A0}', " "));
        }
        lines.push(line);
    }
    Some(lines.join("\n"))
}

fn write_table(table: &Table, document: &FlowDocument, out: &mut String) {
    if table.rows.is_empty() || table.columns.is_empty() {
        return;
    }
    let mut rows = table.rows.iter().map(|row| {
        row.cells
            .iter()
            .map(|cell| cell_text(&cell.blocks, document).replace('|', "\\|"))
            .collect::<Vec<_>>()
    });
    let Some(header) = rows.next() else {
        return;
    };
    out.push('|');
    for cell in &header {
        out.push_str(&format!(" {cell} |"));
    }
    out.push('\n');
    out.push('|');
    for _ in &header {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        out.push('|');
        for cell in &row {
            out.push_str(&format!(" {cell} |"));
        }
        out.push('\n');
    }
}

fn cell_text(blocks: &[Block], document: &FlowDocument) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        if let Block::Paragraph(paragraph) = block {
            parts.push(
                paragraph
                    .runs
                    .iter()
                    .map(|inline| inline_markdown(inline, document))
                    .collect::<String>(),
            );
        }
    }
    parts.join("<br>").replace('\n', "<br>")
}

fn note_text(blocks: &[Block], document: &FlowDocument) -> String {
    cell_text(blocks, document).replace("<br>", " ")
}

fn inline_markdown(inline: &Inline, document: &FlowDocument) -> String {
    match inline {
        Inline::Text(run) => {
            let text = escape_markdown(&run.text);
            let code = is_code_font(&run.style);
            let mut text = if code {
                format!("`{}`", run.text.replace('`', "\\`"))
            } else {
                text
            };
            if run.style.bold && run.style.italic {
                text = format!("***{text}***");
            } else if run.style.bold {
                text = format!("**{text}**");
            } else if run.style.italic {
                text = format!("*{text}*");
            }
            if run.style.strike {
                text = format!("~~{text}~~");
            }
            if run.style.superscript {
                text = format!("<sup>{text}</sup>");
            } else if run.style.subscript {
                text = format!("<sub>{text}</sub>");
            }
            text
        }
        Inline::Link(link) => {
            let text = link
                .runs
                .iter()
                .map(|inline| inline_markdown(inline, document))
                .collect::<String>();
            match &link.target {
                LinkTarget::Uri(uri) => format!("[{text}]({uri})"),
                LinkTarget::Anchor(anchor) => format!("[{text}](#{})", anchor.get()),
                _ => text,
            }
        }
        Inline::Image(image) => {
            let alt = image.alt.as_deref().unwrap_or("");
            match image_data_uri(document, image.image) {
                Some(url) => format!("![{alt}]({url})"),
                None => alt.to_owned(),
            }
        }
        Inline::Note(note) => format!("[^{}]", note.id),
        Inline::Field(field) => match field.kind {
            ofdbox_folio_ir::FieldKind::PageNumber => "{{PAGE}}".to_owned(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn plain_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(run) => escape_markdown(&run.text),
        Inline::Link(link) => link.runs.iter().map(plain_inline).collect(),
        Inline::Image(image) => image.alt.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_code_font(style: &TextStyle) -> bool {
    let name = style.family.name.to_ascii_lowercase();
    name.contains("courier") || name.contains("mono") || name.contains("consol")
}

fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '<' | '>' | '|') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn image_data_uri(document: &FlowDocument, id: AssetId) -> Option<String> {
    let image = document.resources.image(id)?;
    let mime = match image.format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Tiff => "image/tiff",
        _ => "application/octet-stream",
    };
    Some(format!("data:{mime};base64,{}", base64(&image.encoded)))
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(TABLE[usize::from(first >> 2)]));
        out.push(char::from(
            TABLE[usize::from((first & 0b11) << 4 | second >> 4)],
        ));
        out.push(if chunk.len() > 1 {
            char::from(TABLE[usize::from((second & 0b1111) << 2 | third >> 6)])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(TABLE[usize::from(third & 0b11_1111)])
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> FlowDocument {
        read_markdown(text, &MarkdownOptions::default())
    }

    #[test]
    fn parses_headings_and_paragraphs() {
        let flow = parse("# Title\n\nHello **world**.\n\n## Section\n\nText.");
        assert_eq!(flow.metadata.title.as_deref(), Some("Title"));
        let blocks = &flow.sections[0].blocks;
        assert!(matches!(blocks[0], Block::Paragraph(ref p) if p.heading_level == Some(1)));
        assert!(matches!(blocks[2], Block::Paragraph(ref p) if p.heading_level == Some(2)));
    }

    #[test]
    fn parses_emphasis_links_and_code() {
        let flow = parse("A **bold** and *italic* with `code` and [link](https://example.com).");
        let Block::Paragraph(paragraph) = &flow.sections[0].blocks[0] else {
            panic!("paragraph");
        };
        let styles: Vec<_> = paragraph
            .runs
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some(run.style.clone()),
                _ => None,
            })
            .collect();
        assert!(styles.iter().any(|style| style.bold));
        assert!(styles.iter().any(|style| style.italic));
        assert!(
            styles
                .iter()
                .any(|style| style.family.name == "Courier New")
        );
        assert!(
            paragraph
                .runs
                .iter()
                .any(|inline| matches!(inline, Inline::Link(_)))
        );
    }

    #[test]
    fn parses_lists_and_code_blocks() {
        let flow = parse("- one\n- two\n\n```rust\nfn main() {}\n```");
        let blocks = &flow.sections[0].blocks;
        assert!(matches!(blocks[0], Block::Paragraph(_)));
        let Block::Table(table) = &blocks[2] else {
            panic!("code block table");
        };
        let cell = &table.rows[0].cells[0];
        assert!(cell.shading.is_some());
        let Block::Paragraph(code) = &cell.blocks[0] else {
            panic!("code paragraph");
        };
        let Inline::Text(run) = &code.runs[0] else {
            panic!("code run");
        };
        assert!(run.text.contains("fn main()"));
        assert_eq!(run.style.family.name, "Courier New");
    }

    #[test]
    fn code_blocks_keep_blank_lines() {
        let flow = parse("```\nlet a = 1;\n\nlet b = 2;\n```");
        let Block::Table(table) = &flow.sections[0].blocks[0] else {
            panic!("code block table");
        };
        let cell = &table.rows[0].cells[0];
        assert!(cell.shading.is_some());
        assert_eq!(cell.blocks.len(), 3, "two code lines and one blank line");
        let Block::Paragraph(blank) = &cell.blocks[1] else {
            panic!("blank line paragraph");
        };
        assert!(blank.runs.is_empty());
    }

    #[test]
    fn writes_code_blocks_as_fences() {
        let flow = parse("```\nlet a = 1;\n\nlet b = 2;\n```");
        let markdown = write_markdown(&flow);
        assert!(
            markdown.contains("```\nlet a = 1;\n\nlet b = 2;\n```"),
            "{markdown}"
        );
    }

    #[test]
    fn code_blocks_keep_leading_indentation() {
        let flow = parse("```\nfn main() {\n    let x = 1;\n}\n```");
        let Block::Table(table) = &flow.sections[0].blocks[0] else {
            panic!("code block table");
        };
        let cell = &table.rows[0].cells[0];
        let Block::Paragraph(indented) = &cell.blocks[1] else {
            panic!("indented line");
        };
        let Inline::Text(run) = &indented.runs[0] else {
            panic!("code run");
        };
        assert!(
            run.text
                .starts_with("\u{00A0}\u{00A0}\u{00A0}\u{00A0}let x = 1;"),
            "{:?}",
            run.text
        );
        let markdown = write_markdown(&flow);
        assert!(markdown.contains("\n    let x = 1;\n"), "{markdown}");
    }

    #[test]
    fn parses_tables() {
        let flow = parse("| A | B |\n| --- | --- |\n| 1 | 2 |");
        let Block::Table(table) = &flow.sections[0].blocks[0] else {
            panic!("table");
        };
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0].is_header);
    }

    #[test]
    fn missing_image_reports_diagnostic() {
        let flow = parse("![alt](missing.png)");
        assert!(!flow.report.warnings().collect::<Vec<_>>().is_empty());
        assert!(flow.sections[0].blocks.is_empty());
    }

    #[test]
    fn writes_markdown_from_flow() {
        let flow = parse("# Title\n\nHello **bold** and [link](https://example.com).");
        let markdown = write_markdown(&flow);
        assert!(markdown.contains("# Title"), "{markdown}");
        assert!(markdown.contains("**bold**"));
        assert!(markdown.contains("[link](https://example.com)"));
    }

    #[test]
    fn writes_embedded_image_as_data_uri() {
        let mut flow = FlowDocument::new(Metadata::default());
        let image = ImageAsset::new(
            ImageFormat::Png,
            Arc::from(&[0x89, b'P', b'N', b'G'][..]),
            1,
            1,
        );
        let id = flow.resources.add_image(image);
        let mut block = ImageBlock::new(id);
        block.alt = Some("preview".to_owned());
        flow.push(Block::Image(block));
        let markdown = write_markdown(&flow);
        assert!(markdown.contains("![preview](data:image/png;base64,"));
    }
}
