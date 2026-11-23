//! Grafo de cena em memória, componentes/instâncias, variáveis/temas, ops e undo.
//!
//! [`SceneDoc`] owns the schema [`pen_schema::Document`] (source of truth,
//! saved losslessly) and an arena of compact, fully resolved [`SceneNode`]s
//! (instances expanded, overrides/slots applied, `$variables` and themes
//! resolved). Edits go through [`DocOp`]s, which mutate the schema first and
//! then update the scene incrementally, recording dirty [`NodeKey`]s that
//! layout and display-list building consume via [`SceneDoc::take_changes`].

mod build;
pub mod library;
pub mod ops;
pub mod props;
pub mod scene;
pub mod undo;
pub mod vars;

pub use build::{map_blend, parse_weight};
pub use library::{FsLibraries, Library, LibraryResolver, MapLibraries, NoLibraries};
pub use ops::{DocOp, NodeRef, OpError};
pub use pen_render::NodeKey;
pub use props::*;
pub use scene::{Changes, Dirty, SceneDoc};
pub use undo::UndoStack;
pub use vars::{ThemeId, VarVal};
