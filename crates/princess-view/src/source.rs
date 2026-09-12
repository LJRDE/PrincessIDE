//! Source view model — per-line display with inline diagnostics.
//!
//! The source view consumes raw source file content and a list of diagnostics
//! (from `build.diagnostic` events) and produces a per-line display model.
//!
//! # Invariants
//!
//! * Line numbers start at 1 and are strictly sequential.
//! * `lineNo` matches the 1-based index into the input lines.
//! * Each line's `diagnostics` array contains only diagnostics whose `line`
//!   matches that line number.
//! * Diagnostics are sorted by column (ascending).

use princess_core::DiagnosticSeverity;
use serde::Serialize;

/// Severity of a diagnostic, matching the contract's wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticDisplaySeverity {
    Error,
    Warning,
    Note,
}

impl From<DiagnosticSeverity> for DiagnosticDisplaySeverity {
    fn from(s: DiagnosticSeverity) -> Self {
        match s {
            DiagnosticSeverity::Error => Self::Error,
            DiagnosticSeverity::Warning => Self::Warning,
            DiagnosticSeverity::Note => Self::Note,
        }
    }
}

/// A diagnostic attached to a source line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceDiagnostic {
    /// 1-based column number, or `null` when the compiler did not report one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// Severity level.
    pub severity: DiagnosticDisplaySeverity,
    /// Human-readable message.
    pub message: String,
}

/// One row in the source view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    /// 1-based line number.
    pub line_no: u32,
    /// The text of this line (without trailing newline).
    pub text: String,
    /// Diagnostics attached to this line, sorted by column.
    pub diagnostics: Vec<SourceDiagnostic>,
}

/// Source file view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceModel {
    /// The source file path (absolute when known).
    pub file: String,
    /// Per-line display rows.
    pub rows: Vec<SourceRow>,
}

/// Raw diagnostic input for the builder.
#[derive(Debug, Clone)]
pub struct DiagnosticInput {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub col: Option<u32>,
    /// Severity.
    pub severity: DiagnosticSeverity,
    /// Human-readable message.
    pub message: String,
}

/// Builder for [`SourceModel`].
///
/// # Usage
///
/// ```rust
/// use princess_view::SourceBuilder;
///
/// let model = SourceBuilder::new("/path/to/file.c")
///     .content("int main() {\n    return 0;\n}\n")
///     .diagnostics(&[])
///     .build();
/// assert_eq!(model.rows.len(), 3);
/// assert_eq!(model.rows[0].line_no, 1);
/// ```
pub struct SourceBuilder {
    file: String,
    content: String,
    diagnostics: Vec<DiagnosticInput>,
}

impl SourceBuilder {
    /// Create a new builder for the given file path.
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            content: String::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Set the source file content.
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Set the diagnostics to attach to lines.
    pub fn diagnostics(mut self, diagnostics: &[DiagnosticInput]) -> Self {
        self.diagnostics = diagnostics.to_vec();
        self
    }

    /// Build the view model.
    ///
    /// For empty content, returns a model with zero rows.
    pub fn build(self) -> SourceModel {
        let lines: Vec<&str> = if self.content.is_empty() {
            Vec::new()
        } else {
            self.content.lines().collect()
        };

        // Group diagnostics by line number.
        let mut diag_by_line: std::collections::BTreeMap<u32, Vec<SourceDiagnostic>> =
            std::collections::BTreeMap::new();
        for diag in &self.diagnostics {
            let display_severity = DiagnosticDisplaySeverity::from(diag.severity);
            diag_by_line
                .entry(diag.line)
                .or_default()
                .push(SourceDiagnostic {
                    col: diag.col,
                    severity: display_severity,
                    message: diag.message.clone(),
                });
        }

        // Sort diagnostics within each line by column.
        for diags in diag_by_line.values_mut() {
            diags.sort_by_key(|d| d.col.unwrap_or(0));
        }

        let rows: Vec<SourceRow> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let line_no = (i + 1) as u32;
                let diagnostics = diag_by_line
                    .remove(&line_no)
                    .unwrap_or_default();
                SourceRow {
                    line_no,
                    text: line.to_string(),
                    diagnostics,
                }
            })
            .collect();

        SourceModel {
            file: self.file,
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_produces_empty_rows() {
        let model = SourceBuilder::new("test.c").content("").build();
        assert_eq!(model.rows.len(), 0);
    }

    #[test]
    fn line_numbers_start_at_1_and_increment() {
        let model = SourceBuilder::new("test.c")
            .content("first\nsecond\nthird\n")
            .build();
        assert_eq!(model.rows.len(), 3);
        assert_eq!(model.rows[0].line_no, 1);
        assert_eq!(model.rows[1].line_no, 2);
        assert_eq!(model.rows[2].line_no, 3);
    }

    #[test]
    fn diagnostics_attached_to_correct_lines() {
        let diags = vec![
            DiagnosticInput {
                line: 2,
                col: Some(5),
                severity: DiagnosticSeverity::Error,
                message: "expected ';'".into(),
            },
            DiagnosticInput {
                line: 1,
                col: Some(1),
                severity: DiagnosticSeverity::Warning,
                message: "unused variable".into(),
            },
        ];
        let model = SourceBuilder::new("test.c")
            .content("int x;\nreturn 0\n")
            .diagnostics(&diags)
            .build();
        assert_eq!(model.rows[0].diagnostics.len(), 1);
        assert_eq!(model.rows[0].diagnostics[0].message, "unused variable");
        assert_eq!(model.rows[1].diagnostics.len(), 1);
        assert_eq!(model.rows[1].diagnostics[0].message, "expected ';'");
    }
}
