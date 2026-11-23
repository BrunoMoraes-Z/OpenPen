//! Machinery shared by all typed objects: conversion from `serde_json::Value`
//! with lossless fallback, key-order recording and the `pen_object!` macro.

use indexmap::IndexMap;
use serde_json::Value;
use smallvec::SmallVec;

use crate::diag::{DiagCode, Diagnostic};
use crate::json::JsonMap;

/// Token meaning "the discriminant key (`type`) was here".
pub(crate) const TOKEN_TAG: u8 = 0xFE;
/// Token meaning "the next entry of `extra` was here".
pub(crate) const TOKEN_EXTRA: u8 = 0xFF;

/// Compact record of the original key order of an object. Each byte is a field
/// index, [`TOKEN_TAG`] or [`TOKEN_EXTRA`]. Objects created in code have an
/// empty order and serialize in declaration order.
#[derive(Debug, Clone, Default)]
pub struct KeyOrder(SmallVec<[u8; 16]>);

impl KeyOrder {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub(crate) fn push(&mut self, token: u8) {
        self.0.push(token);
    }
    pub(crate) fn tokens(&self) -> &[u8] {
        &self.0
    }
}

/// A value that failed typed conversion.
pub(crate) struct Issue {
    pub key: String,
    pub expected: &'static str,
}

/// Conversion context: collects invalid-value issues and finished diagnostics.
#[derive(Default)]
pub(crate) struct Cx {
    pub issues: Vec<Issue>,
    pub diags: Vec<Diagnostic>,
}

impl Cx {
    pub fn issue(&mut self, key: &str, expected: &'static str) {
        self.issues.push(Issue {
            key: key.to_string(),
            expected,
        });
    }

    /// Prefixes the keys of the issues recorded since `mark` with `prefix.`.
    pub fn prefix_issues(&mut self, mark: usize, prefix: &str) {
        if self.issues.len() > mark {
            for issue in &mut self.issues[mark..] {
                issue.key = format!("{prefix}.{}", issue.key);
            }
        }
    }

    /// Turns the issues recorded since `mark` into diagnostics located at `location`.
    pub fn flush_issues(&mut self, mark: usize, location: &str) {
        if self.issues.len() > mark {
            for issue in self.issues.drain(mark..) {
                let is_sizing = issue.expected == crate::values::Sizing::EXPECTED_STR;
                let code = if is_sizing {
                    DiagCode::InvalidSizing
                } else {
                    DiagCode::InvalidValue
                };
                self.diags.push(Diagnostic::error(
                    code,
                    location,
                    format!(
                        "property '{}' is not {}; kept verbatim",
                        issue.key, issue.expected
                    ),
                ));
            }
        }
    }
}

/// Typed conversion from JSON that never loses data: `accepts` decides whether
/// the value has the expected shape; if not, the caller keeps the raw value.
pub(crate) trait FromJson: Sized {
    /// Human-readable description of the accepted shape.
    const EXPECTED: &'static str;
    fn accepts(v: &Value) -> bool;
    /// Precondition: `Self::accepts(&v)`.
    fn convert(v: Value, cx: &mut Cx) -> Self;
}

impl FromJson for String {
    const EXPECTED: &'static str = "a string";
    fn accepts(v: &Value) -> bool {
        v.is_string()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        match v {
            Value::String(s) => s,
            _ => String::new(),
        }
    }
}

impl FromJson for f64 {
    const EXPECTED: &'static str = "a number";
    fn accepts(v: &Value) -> bool {
        v.is_number()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        v.as_f64().unwrap_or(0.0)
    }
}

impl FromJson for bool {
    const EXPECTED: &'static str = "a boolean";
    fn accepts(v: &Value) -> bool {
        v.is_boolean()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        v.as_bool().unwrap_or(false)
    }
}

impl FromJson for Value {
    const EXPECTED: &'static str = "any JSON value";
    fn accepts(_: &Value) -> bool {
        true
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        v
    }
}

impl FromJson for JsonMap {
    const EXPECTED: &'static str = "an object";
    fn accepts(v: &Value) -> bool {
        v.is_object()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        match v {
            Value::Object(m) => m,
            _ => JsonMap::new(),
        }
    }
}

impl<T: FromJson> FromJson for Vec<T> {
    const EXPECTED: &'static str = "an array of the expected items";
    fn accepts(v: &Value) -> bool {
        v.as_array().is_some_and(|a| a.iter().all(T::accepts))
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        match v {
            Value::Array(a) => a
                .into_iter()
                .enumerate()
                .map(|(i, x)| {
                    let mark = cx.issues.len();
                    let item = T::convert(x, cx);
                    if cx.issues.len() > mark {
                        cx.prefix_issues(mark, &i.to_string());
                    }
                    item
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl<T: FromJson + Copy + Default, const N: usize> FromJson for [T; N] {
    const EXPECTED: &'static str = "a fixed-size array";
    fn accepts(v: &Value) -> bool {
        v.as_array()
            .is_some_and(|a| a.len() == N && a.iter().all(T::accepts))
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        let mut out = [T::default(); N];
        if let Value::Array(a) = v {
            for (slot, x) in out.iter_mut().zip(a) {
                *slot = T::convert(x, cx);
            }
        }
        out
    }
}

impl<T: FromJson> FromJson for IndexMap<String, T> {
    const EXPECTED: &'static str = "an object with values of the expected shape";
    fn accepts(v: &Value) -> bool {
        v.as_object().is_some_and(|m| m.values().all(T::accepts))
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        match v {
            Value::Object(m) => m
                .into_iter()
                .map(|(k, x)| {
                    let mark = cx.issues.len();
                    let item = T::convert(x, cx);
                    cx.prefix_issues(mark, &k);
                    (k, item)
                })
                .collect(),
            _ => IndexMap::new(),
        }
    }
}

/// Visits every `$variable` reference (name without `$`) held by a value.
pub trait VarRefs {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        let _ = f;
    }
}

impl VarRefs for String {}
impl VarRefs for f64 {}
impl VarRefs for bool {}
impl VarRefs for Value {}
impl VarRefs for JsonMap {}

impl<T: VarRefs> VarRefs for Option<T> {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Some(v) = self {
            v.for_each_var(f);
        }
    }
}
impl<T: VarRefs> VarRefs for Vec<T> {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        for v in self {
            v.for_each_var(f);
        }
    }
}
impl<T: VarRefs, const N: usize> VarRefs for [T; N] {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        for v in self {
            v.for_each_var(f);
        }
    }
}
impl<T: VarRefs> VarRefs for IndexMap<String, T> {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        for v in self.values() {
            v.for_each_var(f);
        }
    }
}

/// Defines a string enum with JSON conversion.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $(#[$vmeta:meta])* $var:ident = $s:literal ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $( $(#[$vmeta])* $var ),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),*];
            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),* }
            }
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$var),)* _ => None }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl $crate::object::FromJson for $name {
            const EXPECTED: &'static str = concat!("one of: ", $($s, " "),*);
            fn accepts(v: &serde_json::Value) -> bool {
                v.as_str().is_some_and(|s| Self::from_str(s).is_some())
            }
            fn convert(v: serde_json::Value, _: &mut $crate::object::Cx) -> Self {
                v.as_str().and_then(Self::from_str).unwrap_or($name::ALL[0])
            }
        }

        impl $crate::object::VarRefs for $name {}
    };
}
pub(crate) use str_enum;

/// Defines a typed JSON object: every field is optional (absent fields stay
/// absent on save), unknown/invalid keys go to `extra`, and the original key
/// order is recorded. A companion `#[repr(u8)]` field enum is generated.
macro_rules! pen_object {
    (
        $(#[$meta:meta])*
        pub struct $name:ident : $fenum:ident {
            $( $(#[$fmeta:meta])* $field:ident : $key:literal => $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: Option<$ty>, )*
            /// Unknown keys, and known keys whose value had an invalid shape,
            /// preserved verbatim.
            pub extra: $crate::json::JsonMap,
            /// Original key order (for faithful re-serialization).
            pub order: $crate::object::KeyOrder,
        }

        /// Field identifiers of
        #[doc = concat!("[`", stringify!($name), "`].")]
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum $fenum { $($field),* }

        impl $fenum {
            pub const ALL: &'static [$fenum] = &[$($fenum::$field),*];

            /// JSON key of this field.
            pub const fn key(self) -> &'static str {
                match self { $($fenum::$field => $key),* }
            }

            /// Looks a JSON key up (binary search over a sorted table).
            pub fn from_key(key: &str) -> Option<Self> {
                static TABLE: std::sync::OnceLock<Vec<(&'static str, $fenum)>> = std::sync::OnceLock::new();
                let table = TABLE.get_or_init(|| {
                    let mut v = vec![$(($key, $fenum::$field)),*];
                    v.sort_unstable_by(|a, b| a.0.cmp(b.0));
                    v
                });
                table.binary_search_by(|e| e.0.cmp(key)).ok().map(|i| table[i].1)
            }

            pub const fn bit(self) -> u128 {
                1u128 << (self as u8)
            }
        }

        impl PartialEq for $name {
            /// Semantic equality (ignores the recorded key order).
            fn eq(&self, other: &Self) -> bool {
                $( self.$field == other.$field && )* self.extra == other.extra
            }
        }

        impl $name {
            /// Whether the field is present.
            pub fn has(&self, f: $fenum) -> bool {
                match f { $($fenum::$field => self.$field.is_some()),* }
            }

            /// Removes the field.
            pub fn clear(&mut self, f: $fenum) {
                match f { $($fenum::$field => self.$field = None),* }
            }

            /// Swaps the value of one field with `other`'s (used for
            /// self-inverse property edits).
            pub fn swap_field(&mut self, other: &mut Self, f: $fenum) {
                match f { $($fenum::$field => std::mem::swap(&mut self.$field, &mut other.$field)),* }
            }

            /// Bit set (by `$fenum::bit`) of present fields.
            pub fn present_mask(&self) -> u128 {
                let mut m = 0u128;
                $( if self.$field.is_some() { m |= $fenum::$field.bit(); } )*
                m
            }

            /// Visits `$variable` references in this object's own fields.
            pub fn for_each_var_shallow(&self, f: &mut dyn FnMut(&str)) {
                $( $crate::object::VarRefs::for_each_var(&self.$field, f); )*
            }

            pub(crate) fn from_map(map: $crate::json::JsonMap, tagged: bool, cx: &mut $crate::object::Cx) -> Self {
                use $crate::object::FromJson;
                let mut out = Self::default();
                for (k, v) in map {
                    if tagged && k == "type" {
                        out.order.push($crate::object::TOKEN_TAG);
                        continue;
                    }
                    let Some(f) = $fenum::from_key(&k) else {
                        out.extra.insert(k, v);
                        out.order.push($crate::object::TOKEN_EXTRA);
                        continue;
                    };
                    let rejected = match f {
                        $( $fenum::$field => {
                            if <$ty as FromJson>::accepts(&v) {
                                let mark = cx.issues.len();
                                out.$field = Some(<$ty as FromJson>::convert(v, cx));
                                cx.prefix_issues(mark, $key);
                                None
                            } else {
                                Some((v, <$ty as FromJson>::EXPECTED))
                            }
                        } )*
                    };
                    match rejected {
                        None => out.order.push(f as u8),
                        Some((v, expected)) => {
                            cx.issue(f.key(), expected);
                            out.extra.insert(k, v);
                            out.order.push($crate::object::TOKEN_EXTRA);
                        }
                    }
                }
                out
            }

            pub(crate) fn serialize_tagged<S: serde::Serializer>(
                &self,
                s: S,
                tag: Option<&str>,
            ) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                use $crate::object::{TOKEN_EXTRA, TOKEN_TAG};
                let mut m = s.serialize_map(None)?;
                let mut done = 0u128;
                let mut tag = tag;
                if self.order.is_empty() {
                    if let Some(t) = tag.take() {
                        m.serialize_entry("type", t)?;
                    }
                }
                // Extra entries shadowed by a (later set) typed field are skipped.
                let shadowed = |k: &str| $fenum::from_key(k).is_some_and(|f| self.has(f));
                let mut extras = self.extra.iter();
                for &tok in self.order.tokens() {
                    match tok {
                        TOKEN_TAG => {
                            if let Some(t) = tag.take() {
                                m.serialize_entry("type", t)?;
                            }
                        }
                        TOKEN_EXTRA => {
                            if let Some((k, v)) = extras.next() {
                                if !shadowed(k) {
                                    m.serialize_entry(k, v)?;
                                }
                            }
                        }
                        i => {
                            if let Some(&f) = $fenum::ALL.get(i as usize) {
                                if done & f.bit() == 0 {
                                    done |= f.bit();
                                    match f {
                                        $( $fenum::$field => {
                                            if let Some(v) = &self.$field {
                                                m.serialize_entry($key, v)?;
                                            }
                                        } )*
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(t) = tag.take() {
                    m.serialize_entry("type", t)?;
                }
                $(
                    if done & $fenum::$field.bit() == 0 {
                        if let Some(v) = &self.$field {
                            m.serialize_entry($key, v)?;
                        }
                    }
                )*
                for (k, v) in extras {
                    if !shadowed(k) {
                        m.serialize_entry(k, v)?;
                    }
                }
                m.end()
            }
        }
    };
}
pub(crate) use pen_object;

/// Implements `FromJson`, `Serialize` and `VarRefs` for an untagged `pen_object!`.
macro_rules! plain_object_impls {
    ($($name:ident),* $(,)?) => {$(
        impl $crate::object::FromJson for $name {
            const EXPECTED: &'static str = "an object";
            fn accepts(v: &serde_json::Value) -> bool {
                v.is_object()
            }
            fn convert(v: serde_json::Value, cx: &mut $crate::object::Cx) -> Self {
                match v {
                    serde_json::Value::Object(m) => Self::from_map(m, false, cx),
                    _ => Self::default(),
                }
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.serialize_tagged(s, None)
            }
        }
        impl $crate::object::VarRefs for $name {
            fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
                self.for_each_var_shallow(f);
            }
        }
    )*};
}
pub(crate) use plain_object_impls;
