//! Version migrations 1.0 -> 2.22 on raw JSON (before typed conversion).
//!
//! Each step is a small function that mutates the document in place, exactly
//! like the legacy chain: steps run in sequence, each one rewriting `version`.
//! The walking scope of every step (top-level `children` only, or also the
//! values of `descendants`) reproduces the legacy behavior on purpose, so that
//! documents look the same as in the legacy app after opening.

use serde_json::{Value, json};

use crate::diag::{DiagCode, Diagnostic};
use crate::document::CURRENT_VERSION;
use crate::id::IdGenerator;
use crate::json::{JsonMap, encode_uri_component, js_number_string, js_truthy, number_value};

/// Error that stops the migration chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrateError {
    #[error("document root is not a JSON object")]
    NotAnObject,
    #[error("document has no string \"version\"")]
    MissingVersion,
    #[error("unsupported file format version {0:?} (this build supports up to {CURRENT_VERSION})")]
    UnsupportedVersion(String),
}

/// Mutable state shared by the steps.
pub struct MigrateCx {
    pub ids: IdGenerator,
    pub warnings: Vec<Diagnostic>,
}

impl MigrateCx {
    pub fn new() -> Self {
        MigrateCx {
            ids: IdGenerator::new(),
            warnings: Vec::new(),
        }
    }
}

impl Default for MigrateCx {
    fn default() -> Self {
        Self::new()
    }
}

/// One step of the chain.
pub struct Migration {
    pub from: &'static str,
    pub to: &'static str,
    pub summary: &'static str,
    pub apply: fn(&mut Value, &mut MigrateCx),
}

/// A step that was applied to a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedMigration {
    pub from: &'static str,
    pub to: &'static str,
}

/// The full chain, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        from: "1.0",
        to: "2.0",
        summary: "1.0 node/properties format to 2.0",
        apply: v1_0_to_2_0,
    },
    Migration {
        from: "2.0",
        to: "2.1",
        summary: "frameMaskDisabled -> clip, disabled -> enabled",
        apply: v2_0_to_2_1,
    },
    Migration {
        from: "2.1",
        to: "2.2",
        summary: "ref.overrides -> root props / descendants",
        apply: v2_1_to_2_2,
    },
    Migration {
        from: "2.2",
        to: "2.3",
        summary: "image node -> rectangle with image fill",
        apply: v2_2_to_2_3,
    },
    Migration {
        from: "2.3",
        to: "2.4",
        summary: "layout object -> flat layout props",
        apply: v2_3_to_2_4,
    },
    Migration {
        from: "2.4",
        to: "2.5",
        summary: "text.size -> width/height",
        apply: v2_4_to_2_5,
    },
    Migration {
        from: "2.5",
        to: "2.6",
        summary: "gradient size/center reparametrization",
        apply: v2_5_to_2_6,
    },
    Migration {
        from: "2.6",
        to: "2.7",
        summary: "sweepAngle sign flip",
        apply: v2_6_to_2_7,
    },
    Migration {
        from: "2.7",
        to: "2.8",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.8",
        to: "2.9",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.9",
        to: "2.10",
        summary: "URI-encode import paths",
        apply: v2_9_to_2_10,
    },
    Migration {
        from: "2.10",
        to: "2.11",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.11",
        to: "2.12",
        summary: "icon_font -> icon, stroke object -> flat stroke props",
        apply: v2_11_to_2_12,
    },
    Migration {
        from: "2.12",
        to: "2.13",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.13",
        to: "2.14",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.14",
        to: "2.15",
        summary: "round joins for non-uniform per-side strokes",
        apply: v2_14_to_2_15,
    },
    Migration {
        from: "2.15",
        to: "2.16",
        summary: "groups with layout -> frames",
        apply: v2_15_to_2_16,
    },
    Migration {
        from: "2.16",
        to: "2.17",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.17",
        to: "2.18",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.18",
        to: "2.19",
        summary: "shadows: inner -> outer, drop spread",
        apply: v2_18_to_2_19,
    },
    Migration {
        from: "2.19",
        to: "2.20",
        summary: "image fill modes fill/fit/absent -> cover/contain/stretch",
        apply: v2_19_to_2_20,
    },
    Migration {
        from: "2.20",
        to: "2.21",
        summary: "version bump",
        apply: bump,
    },
    Migration {
        from: "2.21",
        to: "2.22",
        summary: "prompt/context -> note; notes lose text style",
        apply: v2_21_to_2_22,
    },
];

/// Runs the chain until the document reaches [`CURRENT_VERSION`].
pub fn migrate(doc: &mut Value, cx: &mut MigrateCx) -> Result<Vec<AppliedMigration>, MigrateError> {
    let mut applied = Vec::new();
    loop {
        let Value::Object(map) = &*doc else {
            return Err(MigrateError::NotAnObject);
        };
        let version = match map.get("version") {
            Some(Value::String(v)) => v.as_str(),
            _ => return Err(MigrateError::MissingVersion),
        };
        if version == CURRENT_VERSION {
            return Ok(applied);
        }
        let Some(step) = MIGRATIONS.iter().find(|m| m.from == version) else {
            return Err(MigrateError::UnsupportedVersion(version.to_string()));
        };
        (step.apply)(doc, cx);
        if let Value::Object(map) = doc {
            map.insert("version".into(), Value::String(step.to.into()));
        }
        applied.push(AppliedMigration {
            from: step.from,
            to: step.to,
        });
    }
}

// ---------------------------------------------------------------------------
// Walk helpers

/// Legacy node walk: `doc.children` pre-order through `children`, and through
/// the values of `descendants` when `overrides` is set.
fn walk_nodes(doc: &mut Value, overrides: bool, f: &mut dyn FnMut(&mut JsonMap)) {
    if let Some(Value::Array(children)) = doc.get_mut("children") {
        for c in children {
            walk_node(c, overrides, f);
        }
    }
}

fn walk_node(v: &mut Value, overrides: bool, f: &mut dyn FnMut(&mut JsonMap)) {
    let Value::Object(m) = v else { return };
    f(m);
    if let Some(Value::Array(children)) = m.get_mut("children") {
        for c in children {
            walk_node(c, overrides, f);
        }
    }
    if overrides && let Some(Value::Object(d)) = m.get_mut("descendants") {
        for (_, x) in d.iter_mut() {
            walk_node(x, overrides, f);
        }
    }
}

/// Non-null value of a key (`??` semantics).
fn nn<'a>(m: &'a JsonMap, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

/// JavaScript `ToNumber` for the few coercions the legacy code relies on.
fn js_to_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Null => 0.0,
        Value::String(s) if s.trim().is_empty() => 0.0,
        Value::String(s) => s.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// JavaScript `String(v)` for scalars.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => js_number_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

/// JavaScript strict equality for JSON values.
fn js_strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(_), _) | (Value::Object(_), _) => false,
        _ => a == b,
    }
}

fn negate_number(v: &Value) -> Value {
    match v.as_i64() {
        Some(i) if i != i64::MIN => Value::from(-i),
        _ => number_value(-v.as_f64().unwrap_or(0.0)),
    }
}

fn bump(_: &mut Value, _: &mut MigrateCx) {}

// ---------------------------------------------------------------------------
// 1.0 -> 2.0

fn v1_0_to_2_0(doc: &mut Value, cx: &mut MigrateCx) {
    let mut children = Vec::new();
    if let Some(Value::Array(nodes)) = doc.get("children") {
        for n in nodes {
            if let Some(c) = v1_node(n, cx) {
                children.push(c);
            }
        }
    }
    if let Some(Value::Array(conns)) = doc.get("connections") {
        for c in conns {
            let Some(c) = c.as_object() else { continue };
            let id = match c.get("id") {
                Some(v) if js_truthy(Some(v)) => v.clone(),
                _ => Value::String(cx.ids.next_uuid()),
            };
            let endpoint = |path: &str, anchor: &str| {
                let mut e = JsonMap::new();
                if let Some(p) = c.get(path) {
                    e.insert("path".into(), p.clone());
                }
                if let Some(a) = c.get(anchor) {
                    e.insert("anchor".into(), a.clone());
                }
                Value::Object(e)
            };
            let mut m = JsonMap::new();
            m.insert("type".into(), json!("connection"));
            m.insert("id".into(), id);
            m.insert("x".into(), json!(0));
            m.insert("y".into(), json!(0));
            m.insert("source".into(), endpoint("sourceNodeId", "sourceAnchor"));
            m.insert("target".into(), endpoint("targetNodeId", "targetAnchor"));
            children.push(Value::Object(m));
        }
    }
    *doc = json!({ "version": "2.0", "children": children });
}

/// Converts one 1.0 node (`{type, id, properties, children}`).
fn v1_node(t: &Value, cx: &mut MigrateCx) -> Option<Value> {
    let t = t.as_object()?;
    let empty = JsonMap::new();
    let e = t
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let truthy = |k: &str| js_truthy(e.get(k));
    let or0 = |k: &str| nn(e, k).cloned().unwrap_or(json!(0));

    let mut n = JsonMap::new();
    let id = match t.get("id") {
        Some(v) if js_truthy(Some(v)) => v.clone(),
        _ => Value::String(cx.ids.next_uuid()),
    };
    n.insert("id".into(), id);
    n.insert("x".into(), or0("x"));
    n.insert("y".into(), or0("y"));
    for k in ["flipX", "flipY", "disabled"] {
        if truthy(k) {
            n.insert(k.into(), json!(true));
        }
    }
    if truthy("rotation") {
        let rad = js_to_number(&e["rotation"]);
        n.insert("rotation".into(), number_value(-rad.to_degrees()));
    }
    if let Some(op) = nn(e, "opacity")
        && !js_strict_eq(op, &json!(1))
    {
        n.insert("opacity".into(), op.clone());
    }
    if truthy("strokeColor") && truthy("strokeWidth") {
        n.insert("stroke".into(), v1_stroke(e));
    }
    let has_blur = truthy("blurFilter") && js_to_number(&e["blurFilter"]) > 0.0;
    let shadow = e
        .get("dropShadowFilter")
        .and_then(Value::as_object)
        .filter(|s| js_truthy(s.get("enabled")));
    if has_blur || shadow.is_some() {
        let mut effects = Vec::new();
        if has_blur {
            effects.push(json!({ "type": "blur", "radius": e["blurFilter"].clone() }));
        }
        if let Some(s) = shadow {
            let mut rgba = parse_legacy_hex(
                s.get("color")
                    .and_then(Value::as_str)
                    .unwrap_or("#000000ff"),
            );
            rgba[3] *= nn(s, "alpha").map_or(1.0, js_to_number);
            if js_truthy(s.get("color")) {
                let off = s.get("offset").and_then(Value::as_object);
                let coord = |k: &str| off.and_then(|o| nn(o, k)).cloned().unwrap_or(json!(0));
                effects.push(json!({
                    "type": "shadow",
                    "shadowType": "outer",
                    "offset": { "x": coord("x"), "y": coord("y") },
                    "blur": nn(s, "blur").cloned().unwrap_or(json!(0)),
                    "spread": nn(s, "spread").cloned().unwrap_or(json!(0)),
                    "color": rgba_to_legacy_hex(rgba),
                }));
            }
        }
        let effect = if effects.len() == 1 {
            effects.pop().unwrap_or_default()
        } else {
            Value::Array(effects)
        };
        n.insert("effect".into(), effect);
    }
    let has_fill =
        nn(e, "fillColor").is_some_and(|c| c != "transparent") || nn(e, "fillGradient").is_some();
    if has_fill && !truthy("fillGradient") {
        let fill = match nn(e, "fillColor") {
            Some(Value::String(c)) => css_rgb_to_hex(c)
                .map(Value::String)
                .unwrap_or_else(|| json!(c)),
            Some(c) => c.clone(),
            None => json!("#000000"),
        };
        n.insert("fill".into(), fill);
    }

    let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = JsonMap::new();
    let put_type = |out: &mut JsonMap, ty: &str| {
        out.insert("type".into(), json!(ty));
    };
    let size = |out: &mut JsonMap| {
        out.insert("width".into(), or0("width"));
        out.insert("height".into(), or0("height"));
    };
    let copy_truthy = |out: &mut JsonMap, from: &str, to: &str| {
        if truthy(from) {
            out.insert(to.into(), e[from].clone());
        }
    };
    let children = |out: &mut JsonMap, cx: &mut MigrateCx| {
        if let Some(Value::Array(ch)) = t.get("children")
            && !ch.is_empty()
        {
            let mut list = Vec::with_capacity(ch.len());
            for c in ch {
                if let Some(c) = v1_node(c, cx) {
                    list.push(c);
                }
            }
            out.insert("children".into(), Value::Array(list));
        }
    };
    let text_style = |out: &mut JsonMap| {
        copy_truthy(out, "textContent", "content");
        copy_truthy(out, "fontFamily", "fontFamily");
        copy_truthy(out, "fontSize", "fontSize");
        if truthy("fontWeight") {
            out.insert(
                "fontWeight".into(),
                Value::String(js_string(&e["fontWeight"])),
            );
        }
        if truthy("fontStyle") && e["fontStyle"] != "normal" {
            out.insert("fontStyle".into(), e["fontStyle"].clone());
        }
        copy_truthy(out, "letterSpacing", "letterSpacing");
    };

    match ty {
        "frame" => {
            put_type(&mut out, "frame");
            out.extend(n);
            size(&mut out);
            copy_truthy(&mut out, "cornerRadius", "cornerRadius");
            if let Some(v) = e.get("frameMaskDisabled") {
                out.insert("frameMaskDisabled".into(), v.clone());
            }
            children(&mut out, cx);
        }
        "group" => {
            put_type(&mut out, "group");
            out.extend(n);
            children(&mut out, cx);
        }
        "rectangle" => {
            put_type(&mut out, "rectangle");
            out.extend(n);
            size(&mut out);
            copy_truthy(&mut out, "cornerRadius", "cornerRadius");
        }
        "ellipse" | "line" => {
            put_type(&mut out, ty);
            out.extend(n);
            size(&mut out);
        }
        "path" => {
            put_type(&mut out, "path");
            out.extend(n);
            size(&mut out);
            copy_truthy(&mut out, "pathData", "geometry");
        }
        "text" => {
            put_type(&mut out, "text");
            out.extend(n);
            text_style(&mut out);
            copy_truthy(&mut out, "textGrowth", "textGrowth");
            copy_truthy(&mut out, "lineHeight", "lineHeight");
            copy_truthy(&mut out, "textAlign", "textAlign");
            match e.get("textAlignVertical").and_then(Value::as_str) {
                Some("top") => {
                    out.insert("textAlignVertical".into(), json!("top"));
                }
                Some("center" | "middle") => {
                    out.insert("textAlignVertical".into(), json!("middle"));
                }
                Some("bottom") => {
                    out.insert("textAlignVertical".into(), json!("bottom"));
                }
                _ => {}
            }
            match e.get("textGrowth").and_then(Value::as_str) {
                Some("fixed-width") => {
                    out.insert("width".into(), or0("width"));
                }
                Some("fixed-width-height") => size(&mut out),
                _ => {}
            }
        }
        "sticky_note" => {
            put_type(&mut out, "note");
            out.extend(n);
            size(&mut out);
            text_style(&mut out);
            copy_truthy(&mut out, "fillColor", "color");
        }
        "icon_font" => {
            put_type(&mut out, "icon_font");
            out.extend(n);
            size(&mut out);
            copy_truthy(&mut out, "iconFontName", "iconFontName");
            copy_truthy(&mut out, "iconFontFamily", "iconFontFamily");
        }
        other => {
            cx.warnings.push(Diagnostic::warning(
                DiagCode::Migration,
                t.get("id").map(js_string).unwrap_or_default(),
                format!("1.0 node of type {other:?} cannot be converted and was dropped"),
            ));
            return None;
        }
    }
    Some(Value::Object(out))
}

/// 1.0 stroke properties -> the 2.x stroke object `{thickness, join, fill, align, cap}`.
fn v1_stroke(e: &JsonMap) -> Value {
    let mut s = JsonMap::new();
    for (from, to) in [
        ("strokeWidth", "thickness"),
        ("lineJoin", "join"),
        ("strokeColor", "fill"),
    ] {
        if js_truthy(e.get(from)) {
            s.insert(to.into(), e[from].clone());
        }
    }
    let align = e.get("strokeAlignment").and_then(Value::as_f64);
    let align = match align {
        Some(0.0) => Some("outside"),
        Some(0.5) => Some("center"),
        Some(1.0) => Some("inside"),
        _ => None,
    };
    if let Some(a) = align {
        s.insert("align".into(), json!(a));
    }
    if let Some(cap) = e.get("lineCap") {
        s.insert("cap".into(), cap.clone());
    }
    Value::Object(s)
}

/// Legacy hex parser to normalized RGBA (invalid digits read as 0).
fn parse_legacy_hex(s: &str) -> [f64; 4] {
    let h = s.strip_prefix('#').unwrap_or(s);
    let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or(0);
    match h.len() {
        3 => {
            let c = |i: usize| f64::from(hex(&h[i..i + 1]) * 17) / 255.0;
            [c(0), c(1), c(2), 1.0]
        }
        6 => {
            let v = hex(h);
            [
                f64::from((v >> 16) & 255) / 255.0,
                f64::from((v >> 8) & 255) / 255.0,
                f64::from(v & 255) / 255.0,
                1.0,
            ]
        }
        8 => {
            let v = hex(h);
            let c = |shift: u32| f64::from((v >> shift) & 255) / 255.0;
            [c(24), c(16), c(8), c(0)]
        }
        _ => [0.0; 4],
    }
}

/// Normalized RGBA -> `#rrggbb` (opaque) or `#rrggbbaa`.
fn rgba_to_legacy_hex(c: [f64; 4]) -> String {
    let b = |v: f64| {
        let r = (v * 255.0 + 0.5).floor();
        if r.is_nan() {
            0
        } else {
            r.clamp(0.0, 255.0) as u8
        }
    };
    crate::values::Rgba {
        r: b(c[0]),
        g: b(c[1]),
        b: b(c[2]),
        a: b(c[3]),
    }
    .to_hex()
}

/// `rgb(r,g,b)` / `rgba(r,g,b,a)` -> hex, like the legacy 1.0 converter.
fn css_rgb_to_hex(s: &str) -> Option<String> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let inner = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let mut rgb = [0u32; 3];
    for (slot, p) in rgb.iter_mut().zip(&parts) {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
        if *slot > 255 {
            return None;
        }
    }
    let alpha = match parts.get(3) {
        Some(a) => {
            let ok = !a.is_empty()
                && a.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && a.bytes().filter(|b| *b == b'.').count() <= 1
                && !a.ends_with('.');
            if !ok {
                return None;
            }
            a.parse::<f64>().ok()?.clamp(0.0, 1.0)
        }
        None => 1.0,
    };
    let a = (alpha * 255.0 + 0.5).floor() as u32;
    Some(if alpha < 1.0 {
        format!("#{:02x}{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2], a)
    } else {
        format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
    })
}

// ---------------------------------------------------------------------------
// 2.x steps

/// `frameMaskDisabled` -> `clip = !v`, `disabled` -> `enabled = !v`, on every object.
fn v2_0_to_2_1(doc: &mut Value, _: &mut MigrateCx) {
    fn go(v: &mut Value) {
        match v {
            Value::Object(m) => {
                if let Some(old) = m.shift_remove("frameMaskDisabled") {
                    m.insert("clip".into(), Value::Bool(!js_truthy(Some(&old))));
                }
                if let Some(old) = m.shift_remove("disabled") {
                    m.insert("enabled".into(), Value::Bool(!js_truthy(Some(&old))));
                }
                for x in m.values_mut() {
                    go(x);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(go),
            _ => {}
        }
    }
    go(doc);
}

/// `ref.overrides: [{path?, property, value}]` -> root properties or
/// `descendants[path][property]`.
fn v2_1_to_2_2(doc: &mut Value, _: &mut MigrateCx) {
    fn go(v: &mut Value) {
        match v {
            Value::Array(a) => a.iter_mut().for_each(go),
            Value::Object(m)
                if m.get("type").is_some_and(|t| t == "ref") && m.contains_key("overrides") =>
            {
                let overrides = m.shift_remove("overrides");
                let Some(Value::Array(list)) = overrides else {
                    return;
                };
                for o in list {
                    let Value::Object(mut o) = o else { continue };
                    let Some(Value::String(prop)) = o.get("property").cloned() else {
                        continue;
                    };
                    let Some(value) = o.shift_remove("value") else {
                        continue;
                    };
                    match o.get("path") {
                        Some(Value::String(path)) => {
                            let d = m
                                .entry("descendants")
                                .or_insert_with(|| Value::Object(JsonMap::new()));
                            if let Value::Object(d) = d {
                                let target = d
                                    .entry(path.clone())
                                    .or_insert_with(|| Value::Object(JsonMap::new()));
                                if let Value::Object(t) = target {
                                    t.insert(prop, value);
                                }
                            }
                        }
                        Some(_) => continue,
                        None => {
                            m.insert(prop, value);
                        }
                    }
                }
            }
            Value::Object(m) => m.values_mut().for_each(go),
            _ => {}
        }
    }
    go(doc);
}

/// `image` node -> `rectangle` with `fill: {type: image, url, mode: "fill"}`.
/// (Legacy walks `children` only and keeps the node's `url` key.)
fn v2_2_to_2_3(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, false, &mut |n| {
        if n.get("type").is_some_and(|t| t == "image") {
            n.insert("type".into(), json!("rectangle"));
            n.shift_remove("imageUrl");
            let mut fill = JsonMap::new();
            fill.insert("type".into(), json!("image"));
            if let Some(url) = n.get("url") {
                fill.insert("url".into(), url.clone());
            }
            fill.insert("mode".into(), json!("fill"));
            n.insert("fill".into(), Value::Object(fill));
        }
    });
}

/// `layout: {mode, spacing, includeStroke, padding, justify, align}` -> flat
/// properties; frames/groups without layout get `layout: "none"`; frames get
/// `width/height = 0` and `clip = true` when missing. (`children` only.)
fn v2_3_to_2_4(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, false, &mut |n| {
        let old = n.shift_remove("layout");
        let ty = n.get("type").and_then(Value::as_str).unwrap_or("");
        let is_frame = ty == "frame";
        if is_frame || ty == "group" {
            if js_truthy(old.as_ref()) {
                let empty = JsonMap::new();
                let a = old.as_ref().and_then(Value::as_object).unwrap_or(&empty);
                n.insert(
                    "layout".into(),
                    nn(a, "mode").cloned().unwrap_or(json!("none")),
                );
                for (from, to) in [
                    ("spacing", "gap"),
                    ("includeStroke", "layoutIncludeStroke"),
                    ("padding", "padding"),
                    ("justify", "justifyContent"),
                    ("align", "alignItems"),
                ] {
                    // `n.gap = a.spacing`: undefined removes the key.
                    match a.get(from) {
                        Some(v) => {
                            n.insert(to.into(), v.clone());
                        }
                        None => {
                            n.shift_remove(to);
                        }
                    }
                }
            } else {
                n.insert("layout".into(), json!("none"));
            }
        }
        if is_frame {
            for (k, default) in [
                ("width", json!(0)),
                ("height", json!(0)),
                ("clip", json!(true)),
            ] {
                if nn(n, k).is_none() {
                    n.insert(k.into(), default);
                }
            }
        }
    });
}

/// `text.size {width, height}` -> `width` / `height`. (`children` only.)
fn v2_4_to_2_5(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, false, &mut |n| {
        if n.get("type").is_some_and(|t| t == "text") && nn(n, "size").is_some() {
            let size = n.shift_remove("size");
            if let Some(Value::Object(size)) = size {
                for k in ["width", "height"] {
                    if let Some(v) = nn(&size, k) {
                        n.insert(k.into(), v.clone());
                    }
                }
            }
        }
    });
}

/// Gradient reparametrization (`fill` and `stroke.fill`, incl. `descendants`).
fn v2_5_to_2_6(doc: &mut Value, _: &mut MigrateCx) {
    fn gradient(r: &mut Value) {
        let Value::Object(r) = r else { return };
        if r.get("type").is_none_or(|t| t != "gradient") {
            return;
        }
        let size = r.get("size").and_then(Value::as_object);
        let old_w = size
            .and_then(|s| nn(s, "width"))
            .cloned()
            .unwrap_or(json!(1));
        let old_h = size
            .and_then(|s| nn(s, "height"))
            .cloned()
            .unwrap_or(json!(1));
        let center = r.get("center").and_then(Value::as_object);
        let cx = center
            .and_then(|c| nn(c, "x"))
            .and_then(Value::as_f64)
            .unwrap_or(0.5);
        let cy = center
            .and_then(|c| nn(c, "y"))
            .and_then(Value::as_f64)
            .unwrap_or(0.5);
        let linear = r.get("gradientType").is_some_and(|t| t == "linear");
        if nn(r, "size").is_none() {
            r.insert("size".into(), Value::Object(JsonMap::new()));
        }
        let Some(Value::Object(size)) = r.get_mut("size") else {
            return;
        };
        size.insert("width".into(), old_h);
        let new_h = match old_w.as_f64() {
            Some(w) if linear => number_value(w / 2.0),
            _ => old_w,
        };
        size.insert("height".into(), new_h.clone());
        if let (true, Some(rot), Some(h)) = (
            linear,
            r.get("rotation").and_then(Value::as_f64),
            new_h.as_f64(),
        ) {
            let c = (-rot).to_radians();
            let u = h / 2.0;
            r.insert(
                "center".into(),
                json!({ "x": number_value(cx + c.sin() * u), "y": number_value(cy - c.cos() * u) }),
            );
        }
    }
    fn fills(v: &mut Value) {
        match v {
            Value::Array(a) => a.iter_mut().for_each(gradient),
            Value::Object(_) => gradient(v),
            _ => {}
        }
    }
    walk_nodes(doc, true, &mut |n| {
        if let Some(f) = n.get_mut("fill") {
            fills(f);
        }
        if let Some(Value::Object(stroke)) = n.get_mut("stroke")
            && let Some(f) = stroke.get_mut("fill")
        {
            fills(f);
        }
    });
}

/// `sweepAngle` sign flip on ellipses and refs (typed nodes only).
fn v2_6_to_2_7(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, true, &mut |n| {
        let typed = n.get("type").is_some_and(|t| t == "ellipse" || t == "ref");
        if typed
            && let Some(sweep) = n.get_mut("sweepAngle")
            && sweep.is_number()
        {
            *sweep = negate_number(sweep);
        }
    });
}

/// Import URIs get `encodeURIComponent` per path segment (except `pencil:`).
fn v2_9_to_2_10(doc: &mut Value, _: &mut MigrateCx) {
    if let Some(Value::Object(imports)) = doc.get_mut("imports") {
        for v in imports.values_mut() {
            if let Value::String(uri) = v
                && !uri.starts_with("pencil:")
            {
                *uri = uri
                    .split('/')
                    .map(encode_uri_component)
                    .collect::<Vec<_>>()
                    .join("/");
            }
        }
    }
}

/// `icon_font` -> `icon` (`iconFontName` -> `icon`, `iconFontFamily` ->
/// `library`); stroke object -> `stroke` + `strokeWidth` + `strokeLinejoin` +
/// `strokeLinecap` + `strokeAlignment`.
fn v2_11_to_2_12(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, true, &mut |n| {
        if n.get("type").is_some_and(|t| t == "icon_font") {
            n.insert("type".into(), json!("icon"));
        }
        if let Some(v) = n.shift_remove("iconFontName") {
            n.insert("icon".into(), v);
        }
        if let Some(v) = n.shift_remove("iconFontFamily") {
            n.insert("library".into(), v);
        }
        if let Some(Value::Object(_)) = n.get("stroke") {
            let Some(Value::Object(a)) = n.insert("stroke".into(), Value::Null) else {
                return;
            };
            n.insert(
                "stroke".into(),
                nn(&a, "fill").cloned().unwrap_or_else(|| json!([])),
            );
            n.insert(
                "strokeWidth".into(),
                nn(&a, "thickness").cloned().unwrap_or(json!(0)),
            );
            n.insert(
                "strokeLinejoin".into(),
                nn(&a, "join").cloned().unwrap_or(json!("miter")),
            );
            let cap = match a.get("cap").and_then(Value::as_str) {
                Some("round") => "round",
                Some("square") => "square",
                _ => "butt",
            };
            n.insert("strokeLinecap".into(), json!(cap));
            let align = match a.get("align").and_then(Value::as_str) {
                Some("outside") => "outer",
                Some("center") => "center",
                _ => "inner",
            };
            n.insert("strokeAlignment".into(), json!(align));
        }
    });
}

/// Rectangles/frames with non-uniform per-side `strokeWidth` get
/// `strokeLinejoin: "round"`. (`children` only.)
fn v2_14_to_2_15(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, false, &mut |n| {
        let ty = n.get("type").and_then(Value::as_str);
        if !matches!(ty, Some("rectangle" | "frame")) {
            return;
        }
        let Some(Value::Object(w)) = n.get("strokeWidth") else {
            return;
        };
        let side = |k: &str| nn(w, k).cloned().unwrap_or(json!(0));
        let top = side("top");
        let uniform = ["right", "bottom", "left"]
            .iter()
            .all(|k| js_strict_eq(&top, &side(k)));
        if !uniform {
            n.insert("strokeLinejoin".into(), json!("round"));
        }
    });
}

/// Groups with horizontal/vertical layout become frames (`clip: false`); the
/// other groups lose their layout properties.
fn v2_15_to_2_16(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, true, &mut |n| {
        if n.get("type").is_none_or(|t| t != "group") {
            return;
        }
        let flex = n
            .get("layout")
            .is_some_and(|l| l == "horizontal" || l == "vertical");
        if flex {
            n.insert("type".into(), json!("frame"));
            n.insert("clip".into(), json!(false));
        } else {
            for k in [
                "layout",
                "gap",
                "padding",
                "justifyContent",
                "alignItems",
                "layoutIncludeStroke",
            ] {
                n.shift_remove(k);
            }
        }
    });
}

/// Shadows: `shadowType: "inner"` becomes `"outer"` and `spread` is removed.
fn v2_18_to_2_19(doc: &mut Value, _: &mut MigrateCx) {
    fn shadow(e: &mut Value) {
        if let Value::Object(e) = e
            && e.get("type").is_some_and(|t| t == "shadow")
        {
            if e.get("shadowType").is_some_and(|t| t == "inner") {
                e.insert("shadowType".into(), json!("outer"));
            }
            e.shift_remove("spread");
        }
    }
    walk_nodes(doc, true, &mut |n| match n.get_mut("effect") {
        Some(Value::Array(list)) => list.iter_mut().for_each(shadow),
        Some(e) => shadow(e),
        None => {}
    });
}

/// Image fill modes: `fill` -> `cover`, `fit` -> `contain`, absent -> `stretch`.
fn v2_19_to_2_20(doc: &mut Value, _: &mut MigrateCx) {
    fn image(f: &mut Value) {
        let Value::Object(f) = f else { return };
        if f.get("type").is_none_or(|t| t != "image") {
            return;
        }
        let mode = match f.get("mode") {
            Some(Value::String(m)) if m == "fill" => "cover",
            Some(Value::String(m)) if m == "fit" => "contain",
            None => "stretch",
            _ => return,
        };
        f.insert("mode".into(), json!(mode));
    }
    fn fills(v: Option<&mut Value>) {
        match v {
            Some(Value::Array(a)) => a.iter_mut().for_each(image),
            Some(v) => image(v),
            None => {}
        }
    }
    walk_nodes(doc, true, &mut |n| {
        fills(n.get_mut("fill"));
        fills(n.get_mut("stroke"));
    });
}

/// `prompt` -> note (`#AEDEFF`, drops `model`), `context` -> note (`#DBDEE1`);
/// notes lose all text-style properties.
fn v2_21_to_2_22(doc: &mut Value, _: &mut MigrateCx) {
    walk_nodes(doc, true, &mut |n| {
        match n.get("type").and_then(Value::as_str) {
            Some("prompt") => {
                n.insert("type".into(), json!("note"));
                n.insert("color".into(), json!("#AEDEFF"));
                n.shift_remove("model");
            }
            Some("context") => {
                n.insert("type".into(), json!("note"));
                n.insert("color".into(), json!("#DBDEE1"));
            }
            _ => {}
        }
        if n.get("type").is_some_and(|t| t == "note") {
            for k in [
                "fontFamily",
                "fontSize",
                "fontWeight",
                "letterSpacing",
                "fontStyle",
                "underline",
                "lineHeight",
                "textAlign",
                "textAlignVertical",
                "strikethrough",
                "href",
            ] {
                n.shift_remove(k);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::json_eq;
    use pretty_assertions::assert_eq;

    fn cx() -> MigrateCx {
        MigrateCx {
            ids: IdGenerator::with_seed(7),
            warnings: Vec::new(),
        }
    }

    fn step(f: fn(&mut Value, &mut MigrateCx), mut v: Value) -> Value {
        f(&mut v, &mut cx());
        v
    }

    fn assert_json(actual: Value, expected: Value) {
        assert!(
            json_eq(&actual, &expected),
            "\nactual:   {actual}\nexpected: {expected}"
        );
    }

    #[test]
    fn chain_is_contiguous() {
        let mut v = "1.0";
        for m in MIGRATIONS {
            assert_eq!(m.from, v);
            v = m.to;
        }
        assert_eq!(v, CURRENT_VERSION);
    }

    #[test]
    fn runner_reports_steps_and_errors() {
        let mut doc = json!({"version": "2.19", "children": []});
        let applied = migrate(&mut doc, &mut cx()).unwrap();
        assert_eq!(applied.len(), 3);
        assert_eq!(doc["version"], "2.22");
        let mut future = json!({"version": "2.23", "children": []});
        assert_eq!(
            migrate(&mut future, &mut cx()),
            Err(MigrateError::UnsupportedVersion("2.23".into()))
        );
        assert_eq!(
            migrate(&mut json!({"children": []}), &mut cx()),
            Err(MigrateError::MissingVersion)
        );
        assert_eq!(
            migrate(&mut json!([]), &mut cx()),
            Err(MigrateError::NotAnObject)
        );
    }

    #[test]
    fn m1_0_to_2_0() {
        let v = step(
            v1_0_to_2_0,
            json!({
                "version": "1.0",
                "children": [
                    {"type": "frame", "id": "f", "properties": {
                        "x": 10, "width": 100, "rotation": std::f64::consts::FRAC_PI_2,
                        "fillColor": "rgba(255, 0, 0, 0.5)", "frameMaskDisabled": true,
                        "strokeColor": "#000", "strokeWidth": 2, "strokeAlignment": 1, "lineCap": "round",
                        "dropShadowFilter": {"enabled": true, "color": "#ff0000", "alpha": 0.5, "blur": 4}
                     },
                     "children": [
                        {"type": "sticky_note", "properties": {"textContent": "hi", "fillColor": "#ff0"}},
                        {"type": "mystery"}
                     ]}
                ],
                "connections": [{"id": "c1", "sourceNodeId": "f", "sourceAnchor": "top", "targetNodeId": "g", "targetAnchor": "left"}]
            }),
        );
        let f = &v["children"][0];
        assert_eq!(v["version"], "2.0");
        assert_eq!(f["type"], "frame");
        assert_eq!(f["rotation"], json!(-90));
        assert_eq!(f["fill"], "#ff000080");
        assert_eq!(f["frameMaskDisabled"], true);
        assert_json(
            f["stroke"].clone(),
            json!({"thickness": 2, "fill": "#000", "align": "inside", "cap": "round"}),
        );
        assert_json(
            f["effect"].clone(),
            json!({"type": "shadow", "shadowType": "outer", "offset": {"x": 0, "y": 0}, "blur": 4, "spread": 0, "color": "#ff000080"}),
        );
        let note = &f["children"][0];
        assert_eq!(note["type"], "note");
        assert_eq!(note["content"], "hi");
        assert_eq!(note["color"], "#ff0");
        assert_eq!(
            note["id"].as_str().unwrap().len(),
            36,
            "missing ids become UUIDs"
        );
        assert_eq!(
            f["children"].as_array().unwrap().len(),
            1,
            "unknown 1.0 types are dropped"
        );
        assert_json(
            v["children"][1].clone(),
            json!({"type": "connection", "id": "c1", "x": 0, "y": 0,
                   "source": {"path": "f", "anchor": "top"}, "target": {"path": "g", "anchor": "left"}}),
        );
    }

    #[test]
    fn m2_0_to_2_1() {
        let v = step(
            v2_0_to_2_1,
            json!({"children": [{"type": "frame", "frameMaskDisabled": true, "children": [{"disabled": 0}]}]}),
        );
        assert_json(
            v,
            json!({"children": [{"type": "frame", "clip": false, "children": [{"enabled": true}]}]}),
        );
    }

    #[test]
    fn m2_1_to_2_2() {
        let v = step(
            v2_1_to_2_2,
            json!({"children": [{"type": "ref", "ref": "c", "overrides": [
                {"property": "width", "value": 10},
                {"path": "a/b", "property": "content", "value": "x"},
                {"path": "a/b", "property": "fill", "value": "#fff"},
                {"property": 3, "value": 1}
            ]}]}),
        );
        assert_json(
            v,
            json!({"children": [{"type": "ref", "ref": "c", "width": 10,
                   "descendants": {"a/b": {"content": "x", "fill": "#fff"}}}]}),
        );
    }

    #[test]
    fn m2_2_to_2_3() {
        let v = step(
            v2_2_to_2_3,
            json!({"children": [{"type": "image", "url": "./a.png", "imageUrl": "x"}]}),
        );
        assert_json(
            v,
            json!({"children": [{"type": "rectangle", "url": "./a.png",
                   "fill": {"type": "image", "url": "./a.png", "mode": "fill"}}]}),
        );
    }

    #[test]
    fn m2_3_to_2_4() {
        let v = step(
            v2_3_to_2_4,
            json!({"children": [
                {"type": "frame", "layout": {"mode": "vertical", "spacing": 8, "padding": [4, 2], "justify": "center"}},
                {"type": "group", "children": [{"type": "text", "layout": {"mode": "x"}}]},
                {"type": "frame", "width": 5, "clip": false}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "frame", "layout": "vertical", "gap": 8, "padding": [4, 2], "justifyContent": "center",
                 "width": 0, "height": 0, "clip": true},
                {"type": "group", "layout": "none", "children": [{"type": "text"}]},
                {"type": "frame", "width": 5, "clip": false, "layout": "none", "height": 0}
            ]}),
        );
    }

    #[test]
    fn m2_4_to_2_5() {
        let v = step(
            v2_4_to_2_5,
            json!({"children": [{"type": "text", "size": {"width": 10, "height": null}}, {"type": "frame", "size": 1}]}),
        );
        assert_json(
            v,
            json!({"children": [{"type": "text", "width": 10}, {"type": "frame", "size": 1}]}),
        );
    }

    #[test]
    fn m2_5_to_2_6() {
        let v = step(
            v2_5_to_2_6,
            json!({"children": [
                {"type": "frame", "fill": {"type": "gradient", "gradientType": "linear", "rotation": 90,
                                           "size": {"width": 2, "height": 3}}},
                {"type": "ref", "descendants": {"x": {"stroke": {"fill": [{"type": "gradient", "gradientType": "radial",
                                                                            "size": {"width": 0.5}}]}}}}
            ]}),
        );
        let g = &v["children"][0]["fill"];
        assert_json(g["size"].clone(), json!({"width": 3, "height": 1}));
        // rotation 90 => c = -pi/2: x = 0.5 + sin(c) * 0.5 = 0, y = 0.5 - cos(c) * 0.5 = 0.5
        assert!((g["center"]["x"].as_f64().unwrap() - 0.0).abs() < 1e-12);
        assert!((g["center"]["y"].as_f64().unwrap() - 0.5).abs() < 1e-12);
        let r = &v["children"][1]["descendants"]["x"]["stroke"]["fill"][0];
        assert_json(r["size"].clone(), json!({"width": 1, "height": 0.5}));
        assert!(r.get("center").is_none());
    }

    #[test]
    fn m2_6_to_2_7() {
        let v = step(
            v2_6_to_2_7,
            json!({"children": [
                {"type": "ellipse", "sweepAngle": 270},
                {"type": "ref", "sweepAngle": -90.5, "descendants": {"a": {"sweepAngle": 10}, "b": {"type": "ellipse", "sweepAngle": 0}}},
                {"type": "rectangle", "sweepAngle": 5}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "ellipse", "sweepAngle": -270},
                {"type": "ref", "sweepAngle": 90.5, "descendants": {"a": {"sweepAngle": 10}, "b": {"type": "ellipse", "sweepAngle": 0}}},
                {"type": "rectangle", "sweepAngle": 5}
            ]}),
        );
    }

    #[test]
    fn m2_9_to_2_10() {
        let v = step(
            v2_9_to_2_10,
            json!({"imports": {"a": "./my libs/a b.lib.pen", "b": "pencil:halo.lib.pen", "c": 3}}),
        );
        assert_json(
            v,
            json!({"imports": {"a": "./my%20libs/a%20b.lib.pen", "b": "pencil:halo.lib.pen", "c": 3}}),
        );
    }

    #[test]
    fn m2_11_to_2_12() {
        let v = step(
            v2_11_to_2_12,
            json!({"children": [
                {"type": "icon_font", "iconFontName": "check", "iconFontFamily": "lucide"},
                {"type": "frame", "stroke": {"align": "inside", "thickness": 1, "fill": "$--input", "join": "round", "cap": "round"}},
                {"type": "ref", "descendants": {"p": {"stroke": {"align": "outside"}}}},
                {"type": "rectangle", "stroke": "#fff"}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "icon", "icon": "check", "library": "lucide"},
                {"type": "frame", "stroke": "$--input", "strokeWidth": 1, "strokeLinejoin": "round",
                 "strokeLinecap": "round", "strokeAlignment": "inner"},
                {"type": "ref", "descendants": {"p": {"stroke": [], "strokeWidth": 0, "strokeLinejoin": "miter",
                                                      "strokeLinecap": "butt", "strokeAlignment": "outer"}}},
                {"type": "rectangle", "stroke": "#fff"}
            ]}),
        );
    }

    #[test]
    fn m2_14_to_2_15() {
        let v = step(
            v2_14_to_2_15,
            json!({"children": [
                {"type": "frame", "strokeWidth": {"bottom": 1}},
                {"type": "rectangle", "strokeWidth": {"top": 2, "right": 2, "bottom": 2, "left": 2}},
                {"type": "ellipse", "strokeWidth": {"bottom": 1}},
                {"type": "ref", "descendants": {"x": {"type": "frame", "strokeWidth": {"top": 1}}}}
            ]}),
        );
        assert_eq!(v["children"][0]["strokeLinejoin"], "round");
        assert!(v["children"][1].get("strokeLinejoin").is_none());
        assert!(v["children"][2].get("strokeLinejoin").is_none());
        assert!(
            v["children"][3]["descendants"]["x"]
                .get("strokeLinejoin")
                .is_none(),
            "descendants not walked"
        );
    }

    #[test]
    fn m2_15_to_2_16() {
        let v = step(
            v2_15_to_2_16,
            json!({"children": [
                {"type": "group", "layout": "vertical", "gap": 4},
                {"type": "group", "layout": "none", "gap": 4, "padding": 2, "children": [{"type": "group"}]}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "frame", "layout": "vertical", "gap": 4, "clip": false},
                {"type": "group", "children": [{"type": "group"}]}
            ]}),
        );
    }

    #[test]
    fn m2_18_to_2_19() {
        let v = step(
            v2_18_to_2_19,
            json!({"children": [
                {"type": "frame", "effect": {"type": "shadow", "shadowType": "inner", "spread": -2}},
                {"type": "ref", "descendants": {"a": {"effect": [{"type": "shadow", "spread": 1}, {"type": "blur", "spread": 1}]}}}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "frame", "effect": {"type": "shadow", "shadowType": "outer"}},
                {"type": "ref", "descendants": {"a": {"effect": [{"type": "shadow"}, {"type": "blur", "spread": 1}]}}}
            ]}),
        );
    }

    #[test]
    fn m2_19_to_2_20() {
        let v = step(
            v2_19_to_2_20,
            json!({"children": [
                {"type": "rectangle", "fill": [{"type": "image", "mode": "fill"}, {"type": "image", "mode": "fit"}, {"type": "image"}],
                 "stroke": {"type": "image", "mode": "cover"}}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "rectangle", "fill": [{"type": "image", "mode": "cover"}, {"type": "image", "mode": "contain"},
                                               {"type": "image", "mode": "stretch"}],
                 "stroke": {"type": "image", "mode": "cover"}}
            ]}),
        );
    }

    #[test]
    fn m2_21_to_2_22() {
        let v = step(
            v2_21_to_2_22,
            json!({"children": [
                {"type": "prompt", "model": "x", "content": "do", "fontSize": 12},
                {"type": "context", "content": "c"},
                {"type": "note", "fontFamily": "Inter", "color": "#fff"},
                {"type": "text", "fontFamily": "Inter"}
            ]}),
        );
        assert_json(
            v,
            json!({"children": [
                {"type": "note", "content": "do", "color": "#AEDEFF"},
                {"type": "note", "content": "c", "color": "#DBDEE1"},
                {"type": "note", "color": "#fff"},
                {"type": "text", "fontFamily": "Inter"}
            ]}),
        );
    }

    #[test]
    fn version_bumps_only_touch_version() {
        let mut doc = json!({"version": "2.16", "children": [{"type": "frame", "x": 1}]});
        let before = doc["children"].clone();
        let applied = migrate(&mut doc, &mut cx()).unwrap();
        assert_eq!(applied.first().map(|m| m.to), Some("2.17"));
        assert_eq!(doc["children"], before);
    }

    #[test]
    fn legacy_colors() {
        assert_eq!(
            css_rgb_to_hex("rgb(255, 128, 0)").as_deref(),
            Some("#ff8000")
        );
        assert_eq!(
            css_rgb_to_hex("RGBA(0,0,0,.5)").as_deref(),
            Some("#00000080")
        );
        assert_eq!(css_rgb_to_hex("rgb(256,0,0)"), None);
        assert_eq!(rgba_to_legacy_hex(parse_legacy_hex("#abc")), "#aabbcc");
        assert_eq!(
            rgba_to_legacy_hex(parse_legacy_hex("#11223344")),
            "#11223344"
        );
    }
}
