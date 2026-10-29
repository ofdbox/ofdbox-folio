//! Metadata, diagnostics, and conversion reports.
//!
//! The library stays silent: degradable deviations are written to [`Report`], and
//! the caller decides whether to log or display them.

/// A date and time independent of any time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DateTime {
    /// Year.
    pub year: i32,
    /// Month, 1 to 12.
    pub month: u8,
    /// Day, 1 to 31.
    pub day: u8,
    /// Hour.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Second.
    pub second: u8,
    /// Offset from UTC in minutes.
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

/// Document metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Title.
    pub title: Option<String>,
    /// Author.
    pub author: Option<String>,
    /// Subject.
    pub subject: Option<String>,
    /// Keywords.
    pub keywords: Vec<String>,
    /// Creator tool.
    pub creator: Option<String>,
    /// Creation time.
    pub created: Option<DateTime>,
    /// Modification time.
    pub modified: Option<DateTime>,
    /// Document language tag.
    pub language: Option<String>,
}

/// Attachment metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentInfo {
    /// Attachment identifier.
    pub id: super::asset::AttachmentId,
    /// Attachment name.
    pub name: String,
    /// Attachment format description.
    pub format: Option<String>,
    /// Attachment size in bytes.
    pub size: u64,
    /// Creation time.
    pub created: Option<DateTime>,
}

/// Diagnostic severity level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Informational.
    #[default]
    Info,
    /// Warning, indicating that content was approximated or degraded.
    Warning,
    /// Error, indicating that the current item failed but the document can still be processed.
    Error,
}

/// Diagnostic category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiagnosticCode {
    /// Unsupported feature.
    Unsupported,
    /// Unsupported object type.
    UnsupportedObject,
    /// Font missing or unparsable.
    MissingFont,
    /// Font-related diagnostic.
    Font,
    /// Image missing or undecodable.
    Image,
    /// Color or paint not yet implemented.
    Paint,
    /// Invalid geometry data.
    Geometry,
    /// Content was approximated.
    Approximation,
    /// Character-to-glyph mapping is incomplete.
    InvalidMapping,
    /// Page is corrupt.
    CorruptPage,
    /// Resource or size limit exceeded.
    LimitExceeded,
    /// Other problem.
    Other,
}

/// Diagnostic location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Location {
    /// Page index.
    pub page: Option<usize>,
    /// Block index.
    pub block: Option<u32>,
    /// Text item or object index.
    pub item: Option<u32>,
    /// Object identifier in the source format.
    pub object: Option<String>,
}

/// A single diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity level.
    pub severity: Severity,
    /// Category.
    pub code: DiagnosticCode,
    /// Human-readable description.
    pub message: String,
    /// Optional location.
    pub location: Option<Location>,
}

impl Diagnostic {
    /// Create a diagnostic.
    pub fn new(severity: Severity, code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            location: None,
        }
    }

    /// Create an informational diagnostic.
    pub fn info(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }

    /// Create a warning.
    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// Create an error.
    pub fn error(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    /// Attach a location.
    pub fn at(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }
}

/// A collection of diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The diagnostic list, kept in the order produced.
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// Create an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the report is empty.
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Add a diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Add diagnostics in bulk.
    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    /// Iterate over warnings.
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
    }

    /// Iterate over errors.
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
