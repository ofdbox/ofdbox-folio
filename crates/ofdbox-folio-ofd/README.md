English | [简体中文](README.zh-CN.md)

# ofdbox-folio-ofd

OFD read/write adapter for [`ofdbox-folio`](../../README.md): it parses OFD packages into Fixed IR pages and writes Fixed IR back out as an OFD 1.0 package, sharing one package model across both directions.

## Public Entry Points

- `OfdDocument` implements `DocumentSource`: open with `open_path()`, `open_path_with_fonts()`, `open_bytes()` or `open_bytes_with_fonts()`, then use `page_info()`, `load_page()`, `load_attachment()`, `font_names()`, `info()` and `resources()`.
- `write_fixed()` writes a `FixedDocument` with `OfdWriteOptions` (`creator`, optional fixed `doc_id`).
- `probe()` detects an OFD package without parsing pages; `OfdReadOptions` controls annotations, the unsupported-feature policy, image limits, lenient parsing and decryption (`password`, `certificate_private_key`).
- `LenientMode`, `OfdRepair` and `OfdDiagnostic` report the repairs performed while reading; `OfdError` is the adapter error type.

### Example

```rust
use ofdbox_folio::ir::FixedDocument;
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

let bytes = write_fixed(&document, &OfdWriteOptions::default())?;
```

## Coverage

The writer consumes Fixed IR and emits an OFD 1.0 package (`Version="1.0"`). It lives in `ofdbox-folio-ofd` alongside the reader, so both directions share one package model.

### Package Coverage

Paths, text (with `CGTransform` and `DeltaX/DeltaY`), images, group clipping, transparency, axial/radial gradients, font names and embedded font bytes.

The object `Boundary` covers the whole page, with the actual position carried by the `CTM`; line position deltas come directly from glyph-local coordinates, avoiding the deviation introduced by font metrics.

### Fonts

Embedded fonts are subsetted as TrueType based on the characters actually used in the document (glyph IDs are preserved, only unused outlines are cleared, and composite glyph dependencies are kept along with them); when a font cannot be subsetted, it is embedded in full. By default a fixed ZIP timestamp and a content-digest `DocID` are used, so the same input produces the same bytes.

## Known Limitations

- Pattern and mesh fills are not yet written; they fall back to the first color stop.
- Group transparency is distributed to the alpha of child element colors, so independent layer semantics are not preserved.

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — the Fixed IR, `DocumentSource` and resource contracts used here.
- [ofdbox-folio-pdf](../ofdbox-folio-pdf) — the other fixed-layout adapter, writing from the same Fixed IR.
- [ofdbox-folio-render](../ofdbox-folio-render) — rasterizes the pages this crate reads.
