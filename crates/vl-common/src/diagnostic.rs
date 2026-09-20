//! Diagnostics. **All** user-facing errors go through here and render
//! with Ariadne. Stages produce `Vec<Diagnostic>`; only the driver prints.

use ariadne::{sources, Color, Label as ALabel, Report, ReportKind};

use crate::span::Span;

/// A labelled sub-range inside a diagnostic.
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
}

impl Label {
    pub fn new(span: Span) -> Self {
        Self {
            span,
            message: None,
        }
    }

    pub fn with_message(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: Some(message.into()),
        }
    }
}

/// Error vs warning. Warnings never fail the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One compiler diagnostic anchored to spans of a single file.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub labels: Vec<Label>,
    pub note: Option<String>,
    pub code: Option<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            labels: vec![],
            note: None,
            code: None,
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            labels: vec![],
            note: None,
            code: None,
        }
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label::with_message(span, message));
        self
    }

    pub fn with_bare_label(mut self, span: Span) -> Self {
        self.labels.push(Label::new(span));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    fn kind(&self) -> ReportKind<'_> {
        match self.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        }
    }

    fn build_report(&self, filename: &str) -> Report<'_, (String, std::ops::Range<usize>)> {
        let mut builder = Report::build(self.kind(), filename.to_owned(), self.primary_start())
            .with_message(self.message.clone());

        if let Some(code) = &self.code {
            builder = builder.with_code(code.clone());
        }

        for (i, label) in self.labels.iter().enumerate() {
            let mut a = ALabel::new((filename.to_owned(), label.span.range()));
            if let Some(m) = &label.message {
                a = a.with_message(m.clone());
            }
            // First label pops; the rest are context-coloured.
            a = if i == 0 {
                a.with_color(Color::Red)
            } else {
                a.with_color(Color::Yellow)
            };
            builder = builder.with_label(a);
        }

        if let Some(note) = &self.note {
            builder = builder.with_note(note.clone());
        }

        builder.finish()
    }

    fn primary_start(&self) -> usize {
        self.labels.first().map(|l| l.span.start).unwrap_or(0)
    }

    /// Render to a string (used by golden tests).
    pub fn render(&self, filename: &str, source_text: &str) -> String {
        let mut out = Vec::new();
        let report = self.build_report(filename);
        report
            .write(sources([(filename.to_owned(), source_text)]), &mut out)
            .expect("diagnostic rendering must not fail");
        String::from_utf8(out).expect("ariadne output is UTF-8")
    }

    /// Print to stderr with colours. Returns true if this is an error.
    pub fn emit(&self, filename: &str, source_text: &str) -> bool {
        let report = self.build_report(filename);
        // `eprint` colours only when stderr is a TTY; degrades gracefully.
        report
            .eprint(sources([(filename.to_owned(), source_text)]))
            .expect("diagnostic printing must not fail");
        self.is_error()
    }
}

/// Emit a batch; returns true if any diagnostic was an error.
pub fn emit_all(diags: &[Diagnostic], filename: &str, source_text: &str) -> bool {
    let mut failed = false;
    for d in diags {
        failed |= d.emit(filename, source_text);
    }
    failed
}

/// Render a batch to one string (golden-test helper).
pub fn render_all(diags: &[Diagnostic], filename: &str, source_text: &str) -> String {
    diags
        .iter()
        .map(|d| d.render(filename, source_text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_renders_with_ariadne_graphics() {
        let d = Diagnostic::error("unexpected character `@`")
            .with_label(Span::new(4, 5), "here")
            .with_note("identifiers use letters, digits and `_`");
        let out = d.render("demo.vl", "val x = @;\n");
        assert!(out.contains("unexpected character"));
        // Ariadne draws box graphics; guard against silent fallback.
        assert!(out.contains("─") || out.contains("│") || out.contains('|'));
    }
}
