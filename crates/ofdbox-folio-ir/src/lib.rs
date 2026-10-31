//! Public IR and reader contracts for OFDBox Folio.
//!
//! This crate defines format-neutral types such as Flow IR, Fixed IR, geometry and
//! paint, resource handles, the text layer, and [`DocumentSource`]. It does not
//! depend on any document format library and performs no I/O.
//!
//! The current implementation covers the data model, basic geometry operations,
//! resource deduplication, text hit-testing/selection/search, and page validation;
//! layout algorithms, rendering backends, and format adapters live in other crates.

pub mod asset;
pub mod fixed;
pub mod flow;
pub mod font;
pub mod geometry;
pub mod meta;
pub mod paint;
pub mod path;
pub mod source;
pub mod text;
pub mod units;

pub use asset::{
    AnchorId, AssetId, AttachmentId, FontAsset, ImageAsset, ImageFormat, ResourceStore, TemplateId,
    apply_image_mask, detect_image_format, image_dimensions,
};
pub use fixed::{
    Clip, Destination, Element, EngineInfo, FixedDocument, FontFingerprint, Group, ImageElement,
    LinkArea, OutlineItem, Page, PageInfo, PathElement, Scene, TemplateLayer, TemplateZOrder,
    TextElement, ValidationError,
};
pub use flow::{
    Align, Attachment, Block, Border, BorderStyle, Cell, CellBorders, Column, ColumnWidth, Field,
    FieldKind, FloatingObject, FlowDocument, FontFamily, HeaderFooter, HeaderFooterSet, ImageBlock,
    Inline, InlineImage, LineSpacing, Link, LinkTarget, Margins, NamedStyle, Note, NoteKind,
    NoteRef, PageSetup, ParaStyle, Paragraph, PositionFrom, Row, RowHeightRule, Section,
    StyleSheet, TabAlign, TabLeader, TabStop, Table, TableBorders, TextRun, TextStyle, TocBlock,
    TocStyle, VMerge, VerticalAlign, WrapMode,
};
pub use font::{FontError, FontProvider, FontRequest, extract_truetype_face, subset_truetype};
pub use geometry::{Matrix, Point, Rect, Rotation, Size};
pub use meta::{
    AttachmentInfo, DateTime, Diagnostic, DiagnosticCode, Location, Metadata, Report, Severity,
};
pub use paint::{
    FillRule, GradientExtend, GradientStop, LineCap, LineJoin, LinearGradient, Paint,
    RadialGradient, Rgba, StrokePaint, StrokeStyle,
};
pub use path::{Path, PathBuilder, PathSegment};
pub use source::{
    DocumentError, DocumentInfo, DocumentSource, FixedDocumentSource, Limits, PageError,
    SourceFormat,
};
pub use text::{
    Cluster, Glyph, GlyphRun, SearchHit, SearchQuery, Selection, TextItem, TextLayer, TextLine,
    TextPosition, TextRange, hit_test, search, selection_rects, selection_text,
};
pub use units::Twips;
