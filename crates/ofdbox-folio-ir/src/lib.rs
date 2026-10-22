//! OFDBox Folio 的公共 IR 与阅读器契约。
//!
//! 本 crate 定义格式中立的 Flow IR、Fixed IR、几何与画笔、资源句柄、文本层
//! 和 [`DocumentSource`] 等类型，不依赖任何文档格式库，也不做 I/O。
//!
//! 当前实现覆盖数据模型、基础几何运算、资源去重、文本命中/选择/搜索和页面
//! 校验；布局算法、渲染后端和格式适配器分别位于其他 crate。

pub mod asset;
pub mod fixed;
pub mod flow;
pub mod geometry;
pub mod meta;
pub mod paint;
pub mod path;
pub mod source;
pub mod text;
pub mod units;

pub use asset::{
    AnchorId, AssetId, AttachmentId, FontAsset, ImageAsset, ImageFormat, ResourceStore, TemplateId,
};
pub use fixed::{
    Clip, Destination, Element, EngineInfo, FixedDocument, FontFingerprint, Group, ImageElement,
    LinkArea, OutlineItem, Page, PageInfo, PathElement, Scene, TemplateLayer, TemplateZOrder,
    TextElement, ValidationError,
};
pub use flow::{
    Align, Attachment, Block, Border, BorderStyle, Cell, Column, ColumnWidth, Field, FieldKind,
    FlowDocument, FontFamily, HeaderFooter, HeaderFooterSet, ImageBlock, Inline, InlineImage,
    LineSpacing, Link, LinkTarget, Margins, NamedStyle, PageSetup, ParaStyle, Paragraph, Row,
    Section, StyleSheet, Table, TableBorders, TextRun, TextStyle, TocBlock, TocStyle,
};
pub use geometry::{Matrix, Point, Rect, Rotation, Size};
pub use meta::{
    AttachmentInfo, DateTime, Diagnostic, DiagnosticCode, Location, Metadata, Report, Severity,
};
pub use paint::{
    FillRule, GradientExtend, GradientStop, LineCap, LineJoin, LinearGradient, Paint,
    RadialGradient, Rgba, StrokePaint, StrokeStyle,
};
pub use path::{Path, PathBuilder, PathSegment};
pub use source::{DocumentError, DocumentInfo, DocumentSource, Limits, PageError, SourceFormat};
pub use text::{
    Cluster, Glyph, GlyphRun, SearchHit, SearchQuery, Selection, TextItem, TextLayer, TextLine,
    TextPosition, TextRange, hit_test, search, selection_rects, selection_text,
};
pub use units::Twips;
