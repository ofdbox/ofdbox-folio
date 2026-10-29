English | [简体中文](formats.zh-CN.md)

# Format Adapters

The OFD, PDF and DOCX adapters behind the format-neutral IR; for the engine as a whole, start at [`../README.md`](../README.md).

| Adapter | Crate | Directions | Entry points |
|---|---|---|---|
| OFD | `ofdbox-folio-ofd` | read + write | `OfdDocument`, `write_fixed()` |
| PDF | `ofdbox-folio-pdf` | read + write | `PdfDocument`, `write_fixed()` |
| DOCX | `ofdbox-folio-docx` | read + write | `read_flow()`, `write_flow()` |

## OFD Writing

The writer consumes Fixed IR and emits an OFD 1.0 package (`Version="1.0"`). It lives in `ofdbox-folio-ofd` alongside the reader, so both directions share one package model.

### Example

```rust
use ofdbox_folio::ir::FixedDocument;
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

let bytes = write_fixed(&document, &OfdWriteOptions::default())?;
```

### Package coverage

Paths, text (with `CGTransform` and `DeltaX/DeltaY`), images, group clipping, transparency, axial/radial gradients, font names and embedded font bytes.

The object `Boundary` covers the whole page, with the actual position carried by the `CTM`; line position deltas come directly from glyph-local coordinates, avoiding the deviation introduced by font metrics.

### Fonts

Embedded fonts are subsetted as TrueType based on the characters actually used in the document (glyph IDs are preserved, only unused outlines are cleared, and composite glyph dependencies are kept along with them); when a font cannot be subsetted, it is embedded in full. By default a fixed ZIP timestamp and a content-digest `DocID` are used, so the same input produces the same bytes.

### Known limitations

- Pattern and mesh fills are not yet written; they fall back to the first color stop.
- Group transparency is distributed to the alpha of child element colors, so independent layer semantics are not preserved.

## PDF Reading and Writing

`ofdbox-folio-pdf` provides `PdfDocument` (`DocumentSource`) for reading and `write_fixed()` for writing. As with OFD, both directions are driven by the shared Fixed IR.

### Example

```rust
use ofdbox_folio::pdf::{PdfDocument, PdfOptions, PdfReadOptions, write_fixed};

let mut source = PdfDocument::open_path("input.pdf", PdfReadOptions::default())?;
let page = source.load_page(0)?;
let bytes = write_fixed(&fixed, &PdfOptions::default())?;
```

### Reading coverage

Paths, text (Identity-H and simple fonts, `ToUnicode`, `Tr` render mode, character spacing and horizontal scaling), images (including CCITT G3/G4), `sh` shading and PatternType 2 gradient patterns, inline images, Form XObjects, clipping, transparency, page rotation, outlines and attachments, and encrypted PDFs (user or owner password). `PdfDocument` implements `DocumentSource` and provides page sizes, text layers, font names and resource tables.

### Writing coverage

Paths, clipping, images, gradients, transparency groups, text (embedding subsetted TrueType fonts, degrading to glyph outlines on failure), metadata and bookmarks. JPEG is embedded as-is with `DCTDecode`, while PNG/JBIG2/BMP are decoded and embedded with `FlateDecode`.

### Known limitations

- PatternType 1 tiling patterns and JPEG2000 are not yet read.
- TIFF/CCITT image resources are still unsupported when writing; CCITT read from PDF is decoded to PNG first.
- An `/Encrypt` inlined directly in the trailer is first rewritten as an indirect object and forces an xref rebuild, so opening large encrypted files is noticeably slower the first time.

## GPU Backend

The `wgpu` feature of `ofdbox-folio-render` (facade `gpu`) provides the compute backend; `gpu` is off by default.

### Example

```rust
use ofdbox_folio::render::{WgpuBackend, WgpuContext, WgpuTarget};

let backend = WgpuBackend::new(WgpuContext::new_blocking()?)?;
let target = WgpuTarget::new(backend.context(), page.display_size(), 144.0)?;
backend.render_to_target(&page.content, &target)?;
let rgba = target.read_rgba(backend.context())?;
```

### Coverage

Solid fills, strokes, linear gradients, clipping and text outlines.

### Known limitations

- Images, radial gradients and blend modes are not yet implemented.
- Use the CPU renderer when you need the full feature set.

## DOCX Writing

`ofdbox_folio::docx::write_flow()` writes Flow IR into a DOCX package (`[Content_Types].xml`, `document.xml`, `styles.xml`, headers and footers, `footnotes.xml`/`endnotes.xml`, `media/*`, `docProps`), covering sections and page setup (including columns), paragraph and character formatting, tab stops and leaders, tables, inline and floating images, headers and footers, `PAGE`/`NUMPAGES` fields, hyperlinks, footnote and endnote references, automatic TOC fields, bookmarks and outline levels. ZIP timestamps are fixed, so the same input produces the same bytes; `write_flow_with_report()` additionally returns degradation diagnostics. The [main README](../README.md) tracks the current status of DOCX layout and writing.
