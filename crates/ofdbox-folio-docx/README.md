English | [简体中文](README.zh-CN.md)

# ofdbox-folio-docx

DOCX read/write adapter for [`ofdbox-folio`](../../README.md): it parses DOCX packages into Flow IR with `read_flow()` and writes Flow IR back into a DOCX package with `write_flow()`.

## Public Entry Points

- `read_flow()` and `read_flow_path()` return a `FlowDocument` from DOCX bytes or a file; `DocxReadOptions` bounds the size of a single decompressed part.
- `write_flow()` returns the DOCX bytes, `write_flow_with_report()` also returns the degradation diagnostics, and `write_flow_path()` writes straight to a file; all three take `DocxWriteOptions`.
- `probe()` checks whether the bytes are a DOCX (a ZIP containing `word/document.xml`); `DocxError` is the adapter error type.

This crate does not touch Fixed IR and does no layout or rendering; DOCX layout approximates Word but does not promise line-by-line fidelity.

## Coverage

`ofdbox_folio::docx::write_flow()` writes Flow IR into a DOCX package (`[Content_Types].xml`, `document.xml`, `styles.xml`, headers and footers, `footnotes.xml`/`endnotes.xml`, `media/*`, `docProps`), covering sections and page setup (including columns), paragraph and character formatting, tab stops and leaders, tables, inline and floating images, headers and footers, `PAGE`/`NUMPAGES` fields, hyperlinks, footnote and endnote references, automatic TOC fields, bookmarks and outline levels. ZIP timestamps are fixed, so the same input produces the same bytes; `write_flow_with_report()` additionally returns degradation diagnostics. DOCX reading is dispatched by the unified reader (`read_flow` + layout + `FixedDocumentSource`); see [facade & reader](../ofdbox-folio/README.md).

## Known Limitations

- Styles are resolved field by field through the OOXML cascade (document defaults → paragraph style chain → character style chain → direct formatting) and then flattened into direct formatting, so the original named style chain is not reconstructed from a read document.
- Floating images that cannot be represented are downgraded to inline images, and merged cells are treated as ordinary cells; both leave a diagnostic in `FlowDocument.report`, so content is never dropped silently.
- DOCX layout approximates Word rather than promising line-by-line fidelity; table and header/footer fidelity is still being hardened.

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [Reader](../ofdbox-folio/README.md) · [Reader 中文](../ofdbox-folio/README.zh-CN.md) — how the unified reader dispatches DOCX through `read_flow` and layout.
- [ofdbox-folio-ir](../ofdbox-folio-ir) — the Flow IR, `FlowDocument` and diagnostics contracts used here.
- [ofdbox-folio-layout](../ofdbox-folio-layout) — turns the Flow IR read by this crate into Fixed IR pages.
- [ofdbox-folio](../ofdbox-folio) — the facade wiring `read_flow` + layout + `FixedDocumentSource` together.
