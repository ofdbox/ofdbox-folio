English | [简体中文](CONTRACT.zh-CN.md)

# Flow → Fixed Layout Contract (A0)

Updated: 2026-10-31
Scope: the input/output boundary, typesetting rules, and DOCX mapping coverage of `ofdbox-folio-layout`. This document freezes
the layout contract; §12 records the A1–A4 acceptance status.

## 1. Direction and responsibilities

- Only `FlowDocument → FixedDocument` is provided; no reverse direction and no Fixed IR editing API.
- Layout consumes IR only: it does not parse files, access the file system, or render bitmaps. Fonts are injected via
  `FontProvider`, and container and format differences stay in their respective adapter layers.
- Layout results must be deterministic: same input + same font bytes → same output bytes.
- Built-in diagnostics go through `Report`: missing fonts, lines that cannot be broken, oversized images, fields that
  cannot be resolved, etc. must all be recorded; silently producing wrong coordinates is not allowed.

## 2. Units and coordinates

| Item | Convention |
| --- | --- |
| Input length | twip (1/1440 inch) |
| Output length | millimeter (`f64`) |
| Origin | top-left corner of the page, y down |
| Text | glyph coordinates are relative to the baseline; `Glyph::x/y` are local coordinates and `advance` is the step to the next glyph |
| Page box | `PageInfo.media_box == crop_box == (0, 0, w, h)`, `rotation = None` |

Conversion: `mm = twip * 25.4 / 1440`. Layout computes internally with millimeter floats and rounds to
`1e-4 mm` only when writing back to IR, to avoid accumulated error.

## 3. Box model

```text
page ─ margins ─► content area (width W)
  block-level elements are laid out vertically in order: paragraph / image / page break
  paragraph = left indent + available width + right indent
  line box = inline boxes arranged horizontally; the line height is determined by the line-spacing rule
```

- Content area width `W = page.width - margins.left - margins.right` (negative values are treated as 0 and recorded as a diagnostic).
- Paragraph available width `Wp = W - left_indent - right_indent`; the first line additionally subtracts `first_line_indent`.
- The vertical cursor `y` starts at `margins.top`; when `y + line height > page.height - margins.bottom`,
  a page break occurs (the body bottom does not reserve footer height; a tall footer can overlap the body).
- Space before/after: only takes effect within the same page; space-before is dropped at a page break (to avoid a blank top of page).

## 4. Line layout

1. Expand the paragraph's inline content into a sequence of "measurement units": one unit per character, recording width,
   style, and source run index; an inline image is a single unbreakable unit, and a tab is a zero-width breakable unit.
2. Measurement: when `glyph_index(ch)` cannot resolve a glyph, record a diagnostic and use the .notdef width; width =
   `advance_units * size / units_per_em`, without kerning adjustments (no kerning/ligatures).
3. Line-breaking priority:
   - an explicit newline `\n` forces a break;
   - CJK (including full-width punctuation) may break between any characters;
   - Latin text may break after spaces and hyphens, with the space hanging at the end of the line and not counted in the line width;
   - line-start prohibition characters (`，。、；：？！）》」』%`, etc.) pull one preceding character forward; line-end
     prohibition characters (`（《「『`) drag one following character back;
   - when a single unit is wider than the line, it occupies a line of its own without scaling or overflow clipping (scaling rules are deferred to a later milestone).
4. Line height (mm):
   - `LineSpacing::Single` → `font size × (ascent − descent + lineGap)` from the font's `hhea`; fall back to `1.2 × font size` when the font cannot be resolved;
   - `Multiple(f)` → `f ×` the single-spacing height;
   - `Exact(t)` → `t`;
   - `AtLeast(t)` → `max(t, single-spacing height)`.
   Baseline position within the line box: `top + font size × ascent` (the bottom of an inline image when the line contains one); see §4.1.
5. Tabs: width is recorded as 0 during measurement; during placement, the next tab stop is found from the current x (ParaStyle::tabs,
   positions relative to the left edge of the text block), and the text after the tab is placed according to Left/Center/Right/Decimal;
   with no explicit tab stop, advance on integer multiples of a 12.5 mm grid. Leaders (dot/dash/solid) are drawn as one
   stroked line on the baseline, following the text color.
6. Alignment:
   - Left/Center/Right shift the whole line;
   - Justify stretches inline spaces first; when the whole line has no spaces, the remainder is distributed evenly between
     characters (for Chinese lines); the last line is aligned Left;
   - after stretching, word spacing may range from negative to 0 (glyphs are not compressed).

An empty paragraph still occupies one line. When it has no runs, layout uses the default text size for that line, so empty table rows keep a normal row height and their borders.

### 4.1 Line height and font fallback

- Line height: single spacing = font size × (ascent − descent + lineGap), taken from the font's `hhea` (via `ttf-parser`);
  multiple spacing multiplies that by the factor; `Exact(t)` uses `t`, `AtLeast(t)` uses `max(t, single)`. If font resolution fails,
  fall back to `1.2 × font size`.
- Baseline: top of the line box + font size × ascent (aligned to the bottom of an image when the line contains one); it is no longer
  fixed at 0.8 of the line box.
- Per-glyph fallback: when the primary font lacks a glyph, use the fallback font obtained by requesting "no family specified" (usually
  carrying CJK glyphs), taking both width and glyph from the fallback font; when both are missing, still write `.notdef` to preserve
  the text layer and summarize a single missing-glyph diagnostic.

## 5. Pagination and columns

- Paragraphs break across pages by line: a line that does not fit moves to the next column/page to continue; by default at least
  two lines are guaranteed per page (widow/orphan control, equivalent to Word's widow/orphan).
- Columns: `PageSetup::columns` and `column_gap` divide the text block evenly into N columns; the body fills the current column first,
  then flows to the next column, and only starts a new page when the last column is full. Headers and footers still span the full page width.
- `Block::PageBreak` breaks the page immediately; trailing whitespace on the page is preserved.
- A paragraph at the top of a page omits `space_before`.
- Tables break across pages by row: a row that does not fit moves to the next page, and after the break the header rows specified by
  `Table::header_rows` are laid out again.
- Splitting within a table row: when a row is taller than the remaining space on the page, cell content is laid out down to the page
  bottom and the remainder continues on the next page (with header rows laid out again on each page), and a
  `DiagnosticCode::Geometry` diagnostic is recorded.
- Unknown block types are skipped and a `DiagnosticCode::Geometry` diagnostic is recorded; `Toc` is implemented (see §5.3).
- Pagination control: keep_with_next (including chained keep_with_next) and keep_lines_together are implemented; widows/orphans at page
  breaks are handled as "at least two lines per page".

### 5.1 Tables

- Column widths: `ColumnWidth::Twips` is a fixed value, and `ColumnWidth::Weight` distributes the remaining width; when the total exceeds
  the text block width, all widths scale proportionally down to the text block width.
- Merged cells: `Cell.span` (gridSpan) occupies the width of multiple columns; continuation cells of `Cell.v_merge` (vMerge) have empty
  content, are covered by the starting cell above, and skip the top border; the height of a vertical merge is computed from the row
  containing the starting cell (rows are not stretched across boundaries; a known approximation).
- Cells: content is laid out in a sub-box of "column width − left/right padding", with the page bottom as the lower boundary; row height
  is the larger of each cell's content height and `Row::height` (a fixed row height only takes effect once the whole row has been laid out).
- Borders: each cell draws four sides. The outermost ring of the table uses `TableBorders::outer`, internal horizontal lines use
  `horizontal`, and vertical lines use `vertical`; shared edges between adjacent cells are drawn again (same color and width, so
  there is no visual difference).
- Cells may contain nested tables, recursively using the same sub-box logic.

### 5.2 Headers and footers

- The header is laid out downward starting at `header_distance` above the top margin, with the top margin as its lower boundary; when content exceeds
  the top margin, a diagnostic is recorded (it will overlap the body).
- The footer's height is measured first, then it is laid out upward against "page height − `footer_distance`", so the footer is pinned to
  the bottom no matter how tall its content is.
- Headers and footers use the `HeaderFooterSet` of the current section: the section's first page uses `first` (when present), even pages use
  `even` (when present), and the rest use `default`.
- `PAGE`/`NUMPAGES` fields inside headers and footers use the same page numbering as the body; when the total page count is referenced, the whole
  document is laid out twice, with the second pass substituting the page count from the first.
- Only pages with body content are produced: empty pages (for example, two consecutive page breaks) do not leave a blank page
  and do not produce headers or footers on their own.

### 5.3 Automatic table of contents

- `Block::Toc` expands into "a heading + one paragraph per entry": entry text, tab, page number. Paragraphs are left-indented
  by level, and the right tab stop is set at the right edge of the text block, so page numbers are right-aligned.
- Entries and page numbers come from headings registered in the previous layout pass; `layout_with` first runs an empty pass to obtain
  heading page numbers, then substitutes them into the TOC and re-lays out until the TOC entries and total page count are stable (at most 4 passes).
- TOC entry text uses the paragraphs generated during layout; fonts are pre-resolved in `prepare_fonts` against the full document
  character set, which also includes characters from TOC headings.
- Between the entry and the page number is a tab: the right tab stop sits at the right edge of the text block, and when TocStyle::dot_leader
  is enabled a dot leader fills the gap (matching the look of Word's TOC).

### 5.4 Footnotes and endnotes

- `Inline::Note` is the reference in the body; during layout it is numbered in order of appearance (footnotes 1, 2…, endnotes 1, 2…),
  and the mark is set as a small number in the same line.
- Footnotes: after the line containing the reference has been placed, the note body is measured across the full page width, the lower boundary
  of the body on this page is raised immediately (`bottom_inset`), and the note body is queued; at page end, a short horizontal rule one
  third of the width is drawn above the footer and below the body, and then the notes are laid out in numeric order (with a `NOTE_INDENT` left indent).
- When a note pushes body content past the bottom of the page, the line already placed does not roll back, and subsequent lines
  automatically move to the next column/page.
- Endnotes: they do not occupy the page bottom; they are collected at the end of the document and laid out starting on a separate page, first with
  a configurable heading line (`LayoutOptions::endnote_title`, default `"Endnotes"`) and then in order.
- When the note body cannot be found, a `DiagnosticCode::Unsupported` diagnostic is recorded and the note is skipped.

### 5.5 Floating objects

- `Block::Floating` is absolutely positioned in page coordinates (relative to the top-left corner of the text block, in mm) and does not participate
  in the body cursor. A non-page-relative object that does not fit in the remaining page height is deferred to the top of the next page; an object
  taller than one page overflows with a `DiagnosticCode::Geometry` diagnostic.
- The position reference comes from `position_from`: `Page` is relative to the page origin, `Margin`/`Column` are relative to the text block
  (identical in a single-column layout), and `Paragraph` is relative to the anchoring paragraph's top-left corner (its current layout position).
- Floating objects with `behind_text` (`wp:anchor/@behindDoc="1"`) are painted behind the body text.
- When the document contains floating objects, layout runs a probe pass and then a replay pass: the probe records each object's page,
  position and size; the replay pre-places them before the page's body content, so text before the anchor also wraps around them.
  The passes repeat until the plan is stable (at most 4 passes). If the probe pass finds no deferred object and no object starting above the
  body cursor, its result is used directly without a replay pass.
- The width comes from `FloatingObject::width`, defaulting to the full text block; content is laid out within that width inside a bounded box (without pagination).
- Wrapping:
  - `WrapMode::None`: overlay directly, body unchanged;
  - `WrapMode::TopBottom`: the body continues below the object;
  - `WrapMode::Square`: paragraphs that vertically overlap the object wrap **line by line**: each line is re-broken using the available width
    at that moment, lines falling inside the wrap band move to the other side of the object, and the full width is restored once past the object;
    when the object is in the right half of the page, the right margin is reduced; when less than 8 mm remains beside the object, the paragraph
    moves below the object instead of wrapping one character per line;
- Square-wrap exclusion areas are scoped to the page/column container where the floating object was placed; they are cleared
  when the body flow moves to the next column or page, so text on later pages does not wrap around an object from an earlier page.
- Floating objects embedded in a table cell or in a header/footer are treated as ordinary blocks and a diagnostic is recorded.

## 6. Style resolution

- Paragraph: the named style matched by `style_id` along the `based_on` chain (at most 8 levels, cycle-detected) forms the base,
  and **non-default values** in `Paragraph.style` act as direct formatting overrides; without a `style_id` the base is
  `StyleSheet.default_paragraph`. Fields explicitly written as default values in the base style are overridden by direct
  formatting — a known approximation.
- Characters: `TextRun.style` + `TextRun.style_id` work the same way, with the base coming from `StyleSheet.character`
  or `StyleSheet.default_character`.
- The combination rule "paragraph style inherits character style" is not implemented; DOCX reading flattens styles before layout,
  so DOCX input does not need it.

## 7. Fields

- `FieldKind::PageNumber`: the page number of the containing page (1-based).
- `FieldKind::TotalPages`: two-pass layout — the first pass counts total pages and the second substitutes the value.
- `FieldKind::Date`: uses `cached_text`; when missing, it renders as empty and a diagnostic is recorded.
- `FieldKind::Ref`: currently renders `cached_text`; anchor resolution uses heading anchors in document order.

## 8. Images

- Intrinsic size (mm) = pixels / dpi × 25.4; `ImageAsset.dpi` defaults to 96.
- `ImageBlock`: width comes from `size` or the intrinsic width; when it exceeds the content area width, scale proportionally to the content
  area width and record a diagnostic. When the height exceeds the available area, scale proportionally as well; if it still does not fit, break the page.
- `InlineImage`: participates in line layout as an unbreakable unit, aligned to the line baseline (bottom on the baseline).
- An inline image's height is included in the line box: `ascent = max(font ascent, image height)` and
  `height = max(line height, image height + 0.2 × font size)`. Inline images therefore expand the line even under exact line
  spacing (a deliberate deviation from Word, which clips); the `0.2 × font size` term approximates the descent below the baseline.

## 9. Headings and outline

- `Paragraph.heading_level = Some(level)` generates an `OutlineItem` after layout completes:
  the heading text is the paragraph's plain text, and nesting follows `level` (falling back to the corresponding parent when the level is the same or shallower).
- When a heading spans a page break, the outline target points to the page containing the heading's first line.
- The `anchor` is recorded in the document-order index for use by `FieldKind::Ref` and links (currently only resolves it).

## 10. Output mapping

| Flow concept | Fixed IR target |
| --- | --- |
| Paragraph line | `Element::Text(TextElement)` per glyph; each element carries a one-glyph `GlyphRun` (merging into runs is a later optimization) |
| Text layer | one `TextItem` per glyph, plus `TextLine` (items range + bbox + baseline) |
| Inline/block image | `Element::Image(ImageElement)`, with `transform` as a millimeter scale |
| Pagination | one `Page` per page, with `PageInfo` from the page setup |
| Page break | ends the current page and starts a new one |
| Hyperlink | `Page.links` (records the URI and anchor region) |
| Outline | `FixedDocument.outline` |

Font resources: the bytes of every font used are registered as a `FontAsset` in `FixedDocument.resources`, and
`TextItem.font` and `TextElement.font` reference the same `Arc`.

## 11. DOCX support matrix

| DOCX feature | Status | Notes |
| --- | --- | --- |
| Paragraph / character styles, bold italic underline strikethrough, color, font size | Yes | `styles.xml` + `basedOn`, flattened into direct formatting on read |
| Alignment, indentation, line spacing, space before/after | Yes | |
| Sections (page size/margins/columns) | Yes | Columns are laid out (equal-width columns; fill the current column before switching) |
| Tables (column widths, borders, header rows, merged cells) | Yes | gridSpan/vMerge are read and written; vertical merge height approximates from the starting row |
| Images (inline/floating) | Yes | Floating images go into Block::Floating, supporting page/margin/column references and three wrap modes; masked images are baked into alpha when written |
| Headers and footers, page-number fields | Yes | Go into HeaderFooterSet and are laid out (top-pinned/bottom-pinned, PAGE/NUMPAGES fields) |
| Automatic table of contents (TOC field) | Yes | Content controls map to TocBlock, and page numbers are filled in from headings during layout |
| Lists | Partial | Marker text + hanging indent; multi-level numbering and custom bullets are approximations |
| Footnotes/endnotes | Yes | Read and written as separate parts; page-bottom note areas and end-of-document endnotes are both laid out |
| Comments, form controls, SmartArt, VML, embedded objects | No | Recorded as Unsupported diagnostics |

Style policy: on read, cascade field by field according to **presence or absence** in the XML (`docDefaults` → default styles →
`basedOn` chain → direct formatting), and flatten the result into `Paragraph.style` / `TextStyle`, without relying on the IR's
named-style chain. This way, constructs such as `<w:ind w:firstLine="0"/>` that "explicitly write back a default value" can also override non-zero
values in the style, at the cost of `FlowDocument.styles` being empty — only code-constructed Flow documents use
named styles.

## 12. Acceptance

- A1: build a Flow document containing a heading, body text, justification, an inline image, and a page break;
  `layout` outputs N pages; the `ofd_to_png` render matches expectations; the text layer can be searched and selected;
  the heading generates an outline with correct page numbers.
- A2: repeated header rows across table page breaks, automatic TOC page-number fill-in. Current status: tables (column widths, cell sub-boxes, borders,
  header-row repetition across pages), headers and footers (including page-number fields and bottom-pinned footers), and the automatic TOC (two-pass page-number fill-in) are all complete.
- A3: convert real DOCX to OFD and compare page-by-page renders against PDFs exported from Word. Current status: `sample.docx` (headings, multi-format body text, lists, tables, images, footer page numbers) reads 17 blocks, lays out 2 pages, with 0 missing-glyph diagnostics; tables, headers and footers, footnotes, and merged cells are all laid out.
- A4: DOCX → Flow IR → layout → OFD/PDF/bitmap; DOCX writing (including merged cells, image masks, and attachments) and
  `write_flow_with_report` are complete. The CLI lives in the `ofdbox-reader` repository.
