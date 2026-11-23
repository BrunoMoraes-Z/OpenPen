//! Neutral element tree shared by the emitters.

/// One CSS declaration (`prop: value`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Decl {
    pub prop: &'static str,
    pub value: String,
}

/// An HTML element with its styles as CSS declarations.
#[derive(Debug, Clone, Default)]
pub(crate) struct El {
    pub tag: &'static str,
    /// Readable base for the class name (slug of the layer name).
    pub name: String,
    pub decls: Vec<Decl>,
    /// Other attributes, printed in order after `class`.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<El>,
    /// Text content (escaped when printed).
    pub text: Option<String>,
    /// Inner markup lines (inline SVG), printed verbatim, indented.
    pub raw: Vec<String>,
    /// HTML comment printed before the element.
    pub comment: Option<String>,
    /// Class attribute assigned by the emitter.
    pub class: Option<String>,
}

impl El {
    pub fn new(tag: &'static str) -> El {
        El {
            tag,
            ..El::default()
        }
    }

    pub fn set(&mut self, prop: &'static str, value: impl Into<String>) {
        let value = value.into();
        match self.decls.iter_mut().find(|d| d.prop == prop) {
            Some(d) => d.value = value,
            None => self.decls.push(Decl { prop, value }),
        }
    }
}
