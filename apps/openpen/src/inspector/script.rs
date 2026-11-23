//! Model of the inspector's "Script" section (no GPUI): the typed `@input`s
//! declared in a script node's file header, with the values stored on the
//! node, and the conversion of an edit back to the `.pen` value.
//!
//! Pen (`properties.script.*` in the editor bundle) shows one row per input:
//! number field (clamped to `min`/`max`), color swatch, checkbox, multi-line
//! text, select for enums and a component picker for `ref`.

use pen_doc::ParamValue;
use pen_schema as ps;
use pen_script::InputType;

/// Value of one input as the editors show and write it.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    Number(f64),
    Bool(bool),
    /// Strings, enum options, colors (`#hex`) and refs.
    Text(String),
}

/// One `@input` row.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptInput {
    pub name: String,
    pub ty: InputType,
    /// The value the script receives (stored value, variables resolved, or
    /// the header default).
    pub value: ScriptValue,
    /// Variable the stored value is bound to (without `$`).
    pub var: Option<String>,
}

impl ScriptInput {
    /// 0xRRGGBBAA of a color input (black when it does not parse, as Pen).
    pub fn color(&self) -> u32 {
        match &self.value {
            ScriptValue::Text(t) => super::model::parse_hex_color(t).unwrap_or(0x000000ff),
            _ => 0x000000ff,
        }
    }

    pub fn number(&self) -> f64 {
        match self.value {
            ScriptValue::Number(n) => n,
            _ => 0.0,
        }
    }

    pub fn text(&self) -> &str {
        match &self.value {
            ScriptValue::Text(t) => t,
            _ => "",
        }
    }
}

/// The Script section of one node.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptProps {
    /// `scriptUri` as written in the file.
    pub uri: String,
    /// Inputs in header order, or why the header could not be read.
    pub inputs: Result<Vec<ScriptInput>, String>,
}

impl ScriptProps {
    pub fn input(&self, i: usize) -> Option<&ScriptInput> {
        self.inputs.as_ref().ok()?.get(i)
    }
}

fn param_value(v: &ParamValue) -> ScriptValue {
    match v {
        ParamValue::Num(n) => ScriptValue::Number(*n),
        ParamValue::Bool(b) => ScriptValue::Bool(*b),
        ParamValue::Str(s) => ScriptValue::Text(s.to_string()),
        ParamValue::Vec(v) => ScriptValue::Number(v.first().copied().unwrap_or(0.0)),
    }
}

/// What the editor of type `ty` shows for `v` (numbers parsed and clamped,
/// like the script receives them).
fn shown(ty: &InputType, v: ScriptValue) -> ScriptValue {
    match (ty, v) {
        (InputType::Number { min, max }, v) => {
            let n = match v {
                ScriptValue::Number(n) => n,
                ScriptValue::Text(t) => t.trim().parse().unwrap_or(f64::NAN),
                ScriptValue::Bool(_) => f64::NAN,
            };
            let n = if n.is_nan() { min.unwrap_or(0.0) } else { n };
            ScriptValue::Number(clamp(n, *min, *max))
        }
        (InputType::Boolean, ScriptValue::Bool(b)) => ScriptValue::Bool(b),
        (InputType::Boolean, _) => ScriptValue::Bool(false),
        (_, ScriptValue::Text(t)) => ScriptValue::Text(t),
        (_, ScriptValue::Number(n)) => ScriptValue::Text(n.to_string()),
        (_, ScriptValue::Bool(b)) => ScriptValue::Text(b.to_string()),
    }
}

fn clamp(n: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let mut n = n;
    if let Some(m) = min
        && n < m
    {
        n = m;
    }
    if let Some(m) = max
        && n > m
    {
        n = m;
    }
    n
}

/// Builds the section from the script source, the variable an input's
/// stored value is bound to (`stored_var`) and the resolved values.
pub fn script_props(
    uri: &str,
    source: Result<&str, String>,
    stored_var: &dyn Fn(&str) -> Option<String>,
    resolved: &[(std::sync::Arc<str>, ParamValue)],
) -> ScriptProps {
    let inputs = source.and_then(|src| {
        pen_script::parse_header(src)
            .map_err(|e| e.message())
            .map(|h| {
                h.inputs
                    .into_iter()
                    .map(|d| {
                        let var = stored_var(&d.name);
                        let value = resolved
                            .iter()
                            .rev()
                            .find(|(k, _)| **k == *d.name)
                            .map(|(_, v)| param_value(v))
                            .unwrap_or_else(|| {
                                // The header default (a JSON value).
                                let v = &d.default;
                                if let Some(b) = v.as_bool() {
                                    ScriptValue::Bool(b)
                                } else if let Some(n) = v.as_f64() {
                                    ScriptValue::Number(n)
                                } else {
                                    ScriptValue::Text(v.as_str().unwrap_or("").to_string())
                                }
                            });
                        ScriptInput {
                            value: shown(&d.ty, value),
                            name: d.name,
                            ty: d.ty,
                            var,
                        }
                    })
                    .collect()
            })
    });
    ScriptProps {
        uri: uri.to_string(),
        inputs,
    }
}

/// The `.pen` value written for an edit of an input of type `ty` (numbers
/// clamped to the declared range, as Pen's field does).
pub fn input_scalar(ty: &InputType, v: &ScriptValue) -> ps::Scalar {
    match (ty, v) {
        (InputType::Number { min, max }, ScriptValue::Number(n)) => {
            ps::Scalar::Number(clamp(*n, *min, *max))
        }
        (_, ScriptValue::Number(n)) => ps::Scalar::Number(*n),
        (_, ScriptValue::Bool(b)) => ps::Scalar::Bool(*b),
        (_, ScriptValue::Text(t)) => ps::Scalar::Str(ps::StrOrVar(t.clone())),
    }
}

/// Number typed into a number input (`None` = not a number, keep the old
/// value). Accepts the same expressions as the other inspector fields.
pub fn parse_number(ty: &InputType, text: &str, current: f64) -> Option<f64> {
    let v = super::value::parse_number_input(text, current as f32)? as f64;
    Some(match ty {
        InputType::Number { min, max } => clamp(v, *min, *max),
        _ => v,
    })
}

/// Short label of an input type for the row tooltip.
pub fn type_label(ty: &InputType) -> String {
    match ty {
        InputType::Number { min, max } => match (min, max) {
            (Some(a), Some(b)) => format!("number ({a}–{b})"),
            (Some(a), None) => format!("number (≥ {a})"),
            (None, Some(b)) => format!("number (≤ {b})"),
            (None, None) => "number".into(),
        },
        InputType::String => "string".into(),
        InputType::Boolean => "boolean".into(),
        InputType::Color => "color".into(),
        InputType::Ref => "ref".into(),
        InputType::Enum(_) => "enum".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SRC: &str = r#"/**
 * @schema 2.10
 * @input count: number(min=2, max=60) = 12
 * @input color: color = #22C55E
 * @input showGrid: boolean = true
 * @input mode: enum("bars", "lines") = "lines"
 * @input title: string = "Hi"
 */
return [];"#;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn header_defaults_and_stored_values() {
        let resolved: Vec<(Arc<str>, ParamValue)> = vec![
            ("count".into(), ParamValue::Num(100.0)),
            ("color".into(), ParamValue::Str("#FF0000".into())),
            ("other".into(), ParamValue::Num(1.0)),
        ];
        let var = |n: &str| (n == "color").then(|| "accent".to_string());
        let p = script_props("chart.js", Ok(SRC), &var, &resolved);
        let inputs = p.inputs.as_ref().unwrap();
        let names: Vec<&str> = inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["count", "color", "showGrid", "mode", "title"]);
        // Stored values win and numbers are clamped like the script sees them.
        assert_eq!(inputs[0].value, ScriptValue::Number(60.0));
        assert_eq!(inputs[1].color(), 0xff0000ff);
        assert_eq!(inputs[1].var.as_deref(), Some("accent"));
        assert_eq!(inputs[2].value, ScriptValue::Bool(true));
        assert_eq!(inputs[3].text(), "lines");
        assert_eq!(inputs[4].text(), "Hi");
        assert_eq!(type_label(&inputs[0].ty), "number (2–60)");
    }

    #[test]
    fn header_errors_are_shown() {
        let p = script_props("x.js", Ok("/** @input a: nope */"), &none, &[]);
        assert!(p.inputs.unwrap_err().contains("Invalid @input type"));
        let p = script_props("x.js", Err("missing".into()), &none, &[]);
        assert_eq!(p.inputs, Err("missing".into()));
    }

    #[test]
    fn edits_become_pen_values() {
        let num = InputType::Number {
            min: Some(0.0),
            max: Some(10.0),
        };
        assert_eq!(
            input_scalar(&num, &ScriptValue::Number(42.0)),
            ps::Scalar::Number(10.0)
        );
        assert_eq!(parse_number(&num, "3*2", 1.0), Some(6.0));
        assert_eq!(parse_number(&num, "-5", 1.0), Some(0.0));
        assert_eq!(parse_number(&num, "abc", 1.0), None);
        assert_eq!(
            input_scalar(&InputType::Boolean, &ScriptValue::Bool(false)),
            ps::Scalar::Bool(false)
        );
        assert_eq!(
            input_scalar(&InputType::Color, &ScriptValue::Text("#112233".into())),
            ps::Scalar::Str(ps::StrOrVar("#112233".into()))
        );
    }
}
