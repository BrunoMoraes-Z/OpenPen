//! Tipos serde do formato .pen, migrações e validação.
//!
//! Pipeline: [`load_document`] parses JSON (BOM, CRLF and JSONC comments are
//! accepted), runs the migration chain 1.0 -> 2.22 on the raw value
//! ([`migrate`]), converts it into the typed model ([`Document`], [`Node`]) and
//! applies the legacy load-time repairs. [`save_document`] writes it back like
//! `JSON.stringify(doc, null, 2)`.
//!
//! Round-trip is lossless: unknown keys and known keys with invalid values are
//! kept verbatim in each object's `extra`, absent fields stay absent and the
//! original key order is recorded and reproduced.

pub mod diag;
pub mod document;
pub mod id;
pub mod json;
pub mod load;
pub mod migrate;
pub mod node;
mod object;
pub mod paint;
pub mod validate;
pub mod values;

pub use diag::{DiagCode, Diagnostic, Severity};
pub use document::{
    CURRENT_VERSION, Document, DocumentField, FontAxis, FontDef, FontStyleKind, FontWeight,
    ThemedValue, VarScalar, VarType, VarValue, Variable,
};
pub use id::{IdGenerator, is_valid_id, new_id};
pub use load::{
    LineEnding, LoadError, LoadOptions, LoadReport, SaveOptions, document_from_value,
    document_to_value, load_document, load_document_with, load_value_with, node_from_value,
    parse_json, save_document, save_document_to, save_document_with, variable_from_value,
};
pub use migrate::{AppliedMigration, MIGRATIONS, MigrateCx, MigrateError, Migration, migrate};
pub use node::{
    AlignItems, Anchor, Endpoint, FillRule, JustifyContent, LayoutMode, LayoutPosition, LineCap,
    LineJoin, Node, NodeField, NodeKind, Slot, StrokeAlignment, TextAlign, TextAlignVertical,
    TextContent, TextGrowth, TextSpan, allowed_fields,
};
pub use object::{KeyOrder, VarRefs};
pub use paint::{
    BlendMode, BlurEffect, ColorFill, ColorStop, Effect, Effects, Fill, Fills, GradientFill,
    GradientSize, GradientType, ImageFill, ImageMode, MeshGradientFill, MeshHandles, MeshPoint,
    Point, ShaderEffect, ShaderFill, ShadowEffect, ShadowOffset, ShadowType, Uniforms,
};
pub use validate::validate;
pub use values::{
    Bind, BoolOrVar, ColorOrVar, CornerRadius, NumOrVar, OneOrMany, Padding, Rgba, Scalar,
    SideWidths, Sizing, StrOrVar, StrokeWidth, Theme,
};
