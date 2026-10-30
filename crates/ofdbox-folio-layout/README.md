English | [简体中文](README.zh-CN.md)

# ofdbox-folio-layout

`ofdbox-folio-layout` is the one-way typesetting engine of OFDBox Folio. It takes a `FlowDocument` plus a caller-supplied `FontProvider` and returns a paginated `FixedDocument`; layout is the only path from Flow IR to Fixed IR. The rules it implements are frozen in the [layout contract](CONTRACT.md), and its place in the crate graph is described in the [architecture document](../../docs/architecture.md).

## One-Way Direction

```text
FlowDocument ──► layout() / layout_with() ──► FixedDocument ──► writers / renderer
```

- Only Flow → Fixed is provided: no reverse conversion and no Fixed IR editing API.
- Layout consumes IR only. It does not parse files, touch the file system or render bitmaps; fonts arrive through `FontProvider`, and container and format differences stay in their adapter layers.
- Layout results are deterministic: same input + same font bytes + same options produce the same output bytes.
- Problems are recorded, not hidden. Missing fonts, unbreakable lines, oversized images and unresolved fields go into the `Report` diagnostics, because silently producing wrong coordinates is not allowed.

## Entry Points

### `layout`

`layout(document, fonts)` is the short form; it lays out with `LayoutOptions::default()` and only runs extra passes when the document needs them.

### `layout_with`

`layout_with(document, fonts, options)` is the full entry point. It returns `Err(LayoutError::NoSection)` when the document has no sections, and it runs extra passes when the document needs them:

- a dry run to count the total pages for `FieldKind::TotalPages`, with the value substituted in the next pass;
- a dry run that collects heading page numbers for an automatic table of contents, followed by re-layout until the TOC entries and the total page count are stable, at most four passes.

```rust
use ofdbox_folio_layout::{layout, layout_with, LayoutOptions};

let fixed = layout(&flow, &mut fonts)?;

let options = LayoutOptions {
    default_image_dpi: 96.0,
    endnote_title: "Endnotes".to_owned(),
};
let fixed = layout_with(&flow, &mut fonts, options)?;
```

### `LayoutOptions`

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `default_image_dpi` | `f64` | `96.0` | Resolution assumed for an image that declares no DPI. |
| `endnote_title` | `String` | `"Endnotes"` | Heading printed on the endnotes page. |

## What It Lays Out

- **Line breaking**: CJK boundaries, spaces and hyphens are break opportunities; line-start and line-end prohibition rules pull punctuation back; trailing spaces do not consume line width.
- **Paragraphs**: first-line, left and right indents; single, multiple, exact and at-least line spacing; space before and after; left, center, right and justified alignment.
- **Line height and font fallback**: single spacing uses the font's `hhea` ascent, descent and line gap, the baseline follows the ascent, and a missing glyph falls back one character at a time.
- **Pagination and columns**: pages break by line, space before a paragraph is dropped at the top of a page, `PageSetup::columns` flows equal-width columns, and widow/orphan control keeps at least two lines per page.
- **Images**: block images use their declared size or a DPI conversion, are scaled proportionally with a diagnostic when oversized, and inline images are unbreakable units sitting on the baseline.
- **Outlines**: `heading_level` produces an `OutlineItem` tree, targeted at the page and y of the heading's first line.
- **Tables**: fixed and weighted column widths, cell sub-boxes, row-wise page breaks with repeated header rows, continued rows and per-cell borders.
- **Headers and footers**: headers lay out downward from above the top margin, footers are measured and then pinned to the bottom, first-page and even-page variants are supported, and `PAGE`/`NUMPAGES` share the body's numbering.
- **Floating objects**: `Block::Floating` is positioned in page coordinates and supports no-wrap, top-and-bottom and square wrapping.
- **Tab stops**: left, center, right and decimal alignment with dot, dash or solid leaders; without an explicit stop a tab advances on a 12.5 mm grid.
- **Styles**: named styles inherit along the `based_on` chain, and non-default direct formatting overrides them.

## Footnotes, Endnotes and Table of Contents

### Footnotes and Endnotes

- `Inline::Note` is the reference in the body. Notes are numbered in order of appearance (footnotes 1, 2…; endnotes 1, 2…), and the mark is set as a small number in the same line.
- Footnotes reserve the bottom of the referencing page: once the referencing line is placed, the body's lower boundary is raised and the note bodies queue; at page end a horizontal rule one third of the content width is drawn above the footer, and the notes are laid out in numeric order with an 8 mm left indent.
- When a note pushes body content past the page bottom, lines already placed do not roll back; later lines move to the next column or page.
- Endnotes do not occupy the page bottom. They are collected at the end of the document, start on a page of their own with the heading taken from `LayoutOptions::endnote_title` (default `"Endnotes"`), and follow in numeric order.
- A note whose body cannot be found is skipped with a `DiagnosticCode::Unsupported` diagnostic.

### Automatic Table of Contents

- `Block::Toc` expands into a heading plus one paragraph per entry: entry text, a tab, and the page number.
- Entries are left-indented by level, the right tab stop sits at the right edge of the text area so page numbers are right-aligned, and `TocStyle::dot_leader` fills the gap with a dot leader, matching the look of Word's TOC.
- Page numbers come from headings registered in a dry run; `layout_with` substitutes them and re-lays out until the entries and the total page count are stable, at most four passes.
- Fonts for TOC entries are pre-resolved against the document-wide character set, so generated entry text is not drawn as empty boxes.

## Known Approximations

- A vertically merged table cell takes its height from the row that contains the starting cell; rows are not stretched across that boundary.
- Default values written explicitly in a base style are overridden by direct formatting.
- A justified line without spaces distributes the remainder between characters; stretched word spacing may go negative, and glyphs are never compressed.
- A floating object using `PositionFrom::Paragraph` is treated as anchored to the text block, which is identical to `Margin`/`Column` in a single-column layout.
- Floating objects inside table cells or headers and footers are laid out as ordinary blocks, with a diagnostic.
- Widow/orphan control is "at least two lines per page" rather than Word's exact rule.
- There is no kerning and no ligature: width is `advance * size / units_per_em`, and a missing glyph uses the `.notdef` width.
- Line height falls back to `1.2 × font size` when the font metrics cannot be resolved.
- A table row's fixed height only takes effect after the whole row has been laid out.
- Block variants the engine does not handle are skipped with a `DiagnosticCode::Geometry` diagnostic.

## See Also

- [Layout contract](CONTRACT.md): the frozen A0 scope, units, box model, line layout, pagination and output mapping.
- [Architecture](../../docs/architecture.md): the crate map and how layout fits between the two IRs.
- [Project README](../../README.md): goals, status and usage.

## License

Dual-licensed under MIT or Apache-2.0, at your option: [LICENSE-MIT](../../LICENSE-MIT) · [LICENSE-APACHE](../../LICENSE-APACHE).
