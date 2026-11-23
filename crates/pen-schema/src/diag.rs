//! Structured diagnostics shared by loading, migration and validation.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// Machine-readable diagnostic kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagCode {
    /// A known property has a value of the wrong shape; it is preserved verbatim
    /// in `extra` and ignored by the typed model.
    InvalidValue,
    /// `width`/`height` string that is not a valid sizing behavior.
    InvalidSizing,
    /// Property the schema does not know at all (preserved in `extra`).
    UnknownProperty,
    /// Known property that the schema does not allow on this node type.
    PropertyNotAllowed,
    UnknownNodeType,
    MissingNodeType,
    MissingId,
    InvalidId,
    DuplicateId,
    /// `ref` pointing to a node that does not exist in the document.
    DanglingRef,
    /// `ref` node without a `ref` target.
    MissingRef,
    /// Library-qualified reference (`alias:id`) whose alias is not imported.
    UnknownImport,
    /// `descendants` key that does not resolve inside the referenced component.
    DanglingOverridePath,
    /// A component (transitively) instantiates itself.
    RefCycle,
    /// `$name` reference to a variable that is not defined.
    UndefinedVariable,
    InvalidVariable,
    /// Theme axis/value not declared in the document `themes`.
    UnknownTheme,
    MissingRequiredField,
    /// Something was changed while loading (ids regenerated, placeholders dropped...).
    Repaired,
    /// Something noteworthy happened during a migration step.
    Migration,
}

impl DiagCode {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagCode::InvalidValue => "invalid-value",
            DiagCode::InvalidSizing => "invalid-sizing",
            DiagCode::UnknownProperty => "unknown-property",
            DiagCode::PropertyNotAllowed => "property-not-allowed",
            DiagCode::UnknownNodeType => "unknown-node-type",
            DiagCode::MissingNodeType => "missing-node-type",
            DiagCode::MissingId => "missing-id",
            DiagCode::InvalidId => "invalid-id",
            DiagCode::DuplicateId => "duplicate-id",
            DiagCode::DanglingRef => "dangling-ref",
            DiagCode::MissingRef => "missing-ref",
            DiagCode::UnknownImport => "unknown-import",
            DiagCode::DanglingOverridePath => "dangling-override-path",
            DiagCode::RefCycle => "ref-cycle",
            DiagCode::UndefinedVariable => "undefined-variable",
            DiagCode::InvalidVariable => "invalid-variable",
            DiagCode::UnknownTheme => "unknown-theme",
            DiagCode::MissingRequiredField => "missing-required-field",
            DiagCode::Repaired => "repaired",
            DiagCode::Migration => "migration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagCode,
    /// Where the problem is: a node id (or `instance/descendant` path), or a
    /// document-level location such as `variables.--primary`. Empty = document.
    pub location: String,
    pub message: String,
}

impl Diagnostic {
    pub fn new(
        severity: Severity,
        code: DiagCode,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Diagnostic {
            severity,
            code,
            location: location.into(),
            message: message.into(),
        }
    }
    pub fn error(code: DiagCode, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, location, message)
    }
    pub fn warning(
        code: DiagCode,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Severity::Warning, code, location, message)
    }
    pub fn info(code: DiagCode, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, location, message)
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]", self.severity.as_str(), self.code.as_str())?;
        if !self.location.is_empty() {
            write!(f, " {}", self.location)?;
        }
        write!(f, ": {}", self.message)
    }
}
