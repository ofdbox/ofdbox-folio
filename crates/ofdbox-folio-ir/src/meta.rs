//! 元数据、诊断与转换报告。
//!
//! 库保持沉默：可降级的偏差写入 [`Report`]，调用方自行决定记录或展示。

/// 不依赖时区的日期时间。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DateTime {
    /// 年。
    pub year: i32,
    /// 月，1 到 12。
    pub month: u8,
    /// 日，1 到 31。
    pub day: u8,
    /// 时。
    pub hour: u8,
    /// 分。
    pub minute: u8,
    /// 秒。
    pub second: u8,
    /// 相对 UTC 的偏移分钟数。
    pub utc_offset_minutes: i16,
}

impl Default for DateTime {
    fn default() -> Self {
        Self {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            utc_offset_minutes: 0,
        }
    }
}

/// 文档元数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// 标题。
    pub title: Option<String>,
    /// 作者。
    pub author: Option<String>,
    /// 主题。
    pub subject: Option<String>,
    /// 关键词。
    pub keywords: Vec<String>,
    /// 创建工具。
    pub creator: Option<String>,
    /// 创建时间。
    pub created: Option<DateTime>,
    /// 修改时间。
    pub modified: Option<DateTime>,
    /// 文档语言标签。
    pub language: Option<String>,
}

/// 附件元数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentInfo {
    /// 附件标识。
    pub id: super::asset::AttachmentId,
    /// 附件名称。
    pub name: String,
    /// 附件格式说明。
    pub format: Option<String>,
    /// 附件字节数。
    pub size: u64,
    /// 创建时间。
    pub created: Option<DateTime>,
}

/// 诊断严重级别。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Severity {
    /// 提示。
    #[default]
    Info,
    /// 警告，表示内容被近似或降级。
    Warning,
    /// 错误，表示当前项失败但文档可继续处理。
    Error,
}

/// 诊断分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiagnosticCode {
    /// 不支持的特性。
    Unsupported,
    /// 不支持的图元类型。
    UnsupportedObject,
    /// 字体缺失或无法解析。
    MissingFont,
    /// 字体相关诊断。
    Font,
    /// 图片缺失或无法解码。
    Image,
    /// 颜色或画笔暂未实现。
    Paint,
    /// 几何数据无效。
    Geometry,
    /// 内容被近似处理。
    Approximation,
    /// 字符与字形映射不完整。
    InvalidMapping,
    /// 页面损坏。
    CorruptPage,
    /// 超过资源或规模限制。
    LimitExceeded,
    /// 其他问题。
    Other,
}

/// 诊断位置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Location {
    /// 页面索引。
    pub page: Option<usize>,
    /// 块索引。
    pub block: Option<u32>,
    /// 文本项或对象索引。
    pub item: Option<u32>,
    /// 源格式中的对象标识。
    pub object: Option<String>,
}

/// 一条诊断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 严重级别。
    pub severity: Severity,
    /// 分类。
    pub code: DiagnosticCode,
    /// 可读说明。
    pub message: String,
    /// 可选位置。
    pub location: Option<Location>,
}

impl Diagnostic {
    /// 创建一条诊断。
    pub fn new(severity: Severity, code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            location: None,
        }
    }

    /// 创建提示。
    pub fn info(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }

    /// 创建警告。
    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// 创建错误。
    pub fn error(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    /// 附加位置。
    pub fn at(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }
}

/// 诊断集合。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 诊断列表，按产生顺序保存。
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// 创建空报告。
    pub fn new() -> Self {
        Self::default()
    }

    /// 报告是否为空。
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// 添加诊断。
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// 批量添加诊断。
    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    /// 遍历警告。
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
    }

    /// 遍历错误。
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, DiagnosticCode, Report, Severity};

    #[test]
    fn report_filters_warnings_and_errors() {
        let mut report = Report::new();
        report.push(Diagnostic::info(DiagnosticCode::Other, "info"));
        report.push(Diagnostic::warning(DiagnosticCode::Unsupported, "warn"));
        report.push(Diagnostic::error(DiagnosticCode::CorruptPage, "error"));
        assert_eq!(report.warnings().count(), 1);
        assert_eq!(report.errors().count(), 1);
        assert!(!report.is_empty());
        assert_eq!(report.diagnostics[1].severity, Severity::Warning);
    }
}
