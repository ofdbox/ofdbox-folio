English | [简体中文](README.zh-CN.md)

# ofdbox-folio-pdf

PDF read/write adapter for [`ofdbox-folio`](../../README.md): it parses PDF pages into Fixed IR and writes Fixed IR back out as PDF, with both directions driven by the shared Fixed IR.

## Public Entry Points

- `PdfDocument` implements `DocumentSource`: open with `open_path()`, `open_path_with_fonts()`, `open_bytes()` or `open_bytes_with_fonts()`, then use `page_info()`, `load_page()`, `load_attachment()`, `font_names()`, `info()` and `resources()`. Encrypted files take a user or owner password through `PdfReadOptions`.
- `write_fixed()` writes a `FixedDocument` with `PdfOptions`; `Writer` exposes the incremental path (`set_metadata()`, `add_outline()`, `add_page()`, `add_scene()`, `finish()`).
- `probe()` detects a PDF header; `PdfReadOptions` bounds page streams, image pixels and Form XObject recursion. `PdfError` is the adapter error type.

### Example

```rust
use ofdbox_folio::pdf::{PdfDocument, PdfOptions, PdfReadOptions, write_fixed};

let mut source = PdfDocument::open_path("input.pdf", PdfReadOptions::default())?;
let page = source.load_page(0)?;
let bytes = write_fixed(&fixed, &PdfOptions::default())?;
```

## Coverage

`ofdbox-folio-pdf` provides `PdfDocument` (`DocumentSource`) for reading and `write_fixed()` for writing. As with OFD, both directions are driven by the shared Fixed IR.

### Reading Coverage

Paths, text (Identity-H and simple fonts, `ToUnicode`, `Tr` render mode, character spacing and horizontal scaling), images (including CCITT G3/G4), `sh` shading and PatternType 2 gradient patterns, inline images, Form XObjects, clipping, transparency, page rotation, outlines and attachments, and encrypted PDFs (user or owner password). `PdfDocument` implements `DocumentSource` and provides page sizes, text layers, font names and resource tables.

### Writing Coverage

Paths, clipping, images, gradients, transparency groups, text (embedding subsetted TrueType fonts, degrading to glyph outlines on failure), metadata and bookmarks. JPEG is embedded as-is with `DCTDecode`, while PNG/JBIG2/BMP are decoded and embedded with `FlateDecode`.

## Known Limitations

- PatternType 1 tiling patterns and JPEG2000 are not yet read.
- TIFF/CCITT image resources are still unsupported when writing; CCITT read from PDF is decoded to PNG first.
- An `/Encrypt` inlined directly in the trailer is first rewritten as an indirect object and forces an xref rebuild, so opening large encrypted files is noticeably slower the first time.

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — the Fixed IR, `DocumentSource` and resource contracts used here.
- [ofdbox-folio-ofd](../ofdbox-folio-ofd) — the other fixed-layout adapter, writing from the same Fixed IR.
- [ofdbox-folio-render](../ofdbox-folio-render) — rasterizes the pages this crate reads.
