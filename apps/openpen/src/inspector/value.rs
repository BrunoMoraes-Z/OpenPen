//! Pure value logic behind the inspector fields: parsing and formatting of
//! numbers, sizes (`fit_content` / `fill_container`), simple arithmetic in
//! fields, and the scrub (drag-on-label) math. No GPUI here.

use std::fmt;

/// Width/height of a node as stored in `.pen` (`number`, `"fit_content(…)"`,
/// `"fill_container(…)"` or a `"$variable"`).
#[derive(Clone, Debug, PartialEq)]
pub enum SizeValue {
    Fixed(f32),
    /// Hug contents; optional fallback size used when there is no content.
    FitContent(Option<f32>),
    /// Fill the parent's free space; optional fallback size.
    FillContainer(Option<f32>),
    Variable(String),
}

impl SizeValue {
    /// Parses the `.pen` schema form.
    pub fn parse_schema(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(v) = s.strip_prefix('$') {
            return (!v.is_empty()).then(|| Self::Variable(v.to_string()));
        }
        for (kw, make) in [
            ("fit_content", Self::FitContent as fn(Option<f32>) -> Self),
            (
                "fill_container",
                Self::FillContainer as fn(Option<f32>) -> Self,
            ),
        ] {
            if let Some(rest) = s.strip_prefix(kw) {
                let rest = rest.trim();
                if rest.is_empty() {
                    return Some(make(None));
                }
                let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
                return inner
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| make(Some(v)));
            }
        }
        s.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(Self::Fixed)
    }

    /// Serializes to the `.pen` schema form.
    pub fn to_schema(&self) -> String {
        match self {
            Self::Fixed(v) => format_number(*v),
            Self::FitContent(None) => "fit_content".into(),
            Self::FitContent(Some(v)) => format!("fit_content({})", format_number(*v)),
            Self::FillContainer(None) => "fill_container".into(),
            Self::FillContainer(Some(v)) => format!("fill_container({})", format_number(*v)),
            Self::Variable(v) => format!("${v}"),
        }
    }

    pub fn mode(&self) -> SizeMode {
        match self {
            Self::Fixed(_) | Self::Variable(_) => SizeMode::Fixed,
            Self::FitContent(_) => SizeMode::Hug,
            Self::FillContainer(_) => SizeMode::Fill,
        }
    }

    /// The number a scrub starts from / the field shows next to the mode.
    pub fn numeric(&self, resolved: f32) -> f32 {
        match self {
            Self::Fixed(v) => *v,
            Self::FitContent(v) | Self::FillContainer(v) => v.unwrap_or(resolved),
            Self::Variable(_) => resolved,
        }
    }

    /// Parses what the user typed in a W/H field. Keywords switch the mode,
    /// anything else is evaluated as arithmetic (relative to `current`).
    pub fn parse_user(input: &str, current: f32) -> Option<Self> {
        let t = input.trim().to_ascii_lowercase();
        match t.as_str() {
            "fill" | "fill container" | "fill_container" => return Some(Self::FillContainer(None)),
            "hug" | "fit" | "auto" | "fit content" | "fit_content" => {
                return Some(Self::FitContent(None));
            }
            _ => {}
        }
        if let Some(v) = Self::parse_schema(input)
            && !matches!(v, Self::Fixed(_))
        {
            return Some(v);
        }
        parse_number_input(input, current).map(|v| Self::Fixed(v.max(0.0)))
    }

    /// Text shown inside the W/H field.
    pub fn display(&self, resolved: f32) -> String {
        match self {
            Self::Fixed(v) => format_number(*v),
            Self::Variable(v) => format!("${v}"),
            // Pen shows the resolved number and the mode in the trailing menu.
            Self::FitContent(_) | Self::FillContainer(_) => format_number(resolved),
        }
    }
}

/// Sizing mode picked from the W/H trailing dropdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeMode {
    Fixed,
    Hug,
    Fill,
}

impl SizeMode {
    pub const ALL: [SizeMode; 3] = [SizeMode::Fixed, SizeMode::Hug, SizeMode::Fill];

    pub fn label(self) -> &'static str {
        match self {
            Self::Fixed => "Fixed",
            Self::Hug => "Hug",
            Self::Fill => "Fill",
        }
    }

    /// Converts `v` to this mode, keeping the current number as fallback.
    pub fn apply(self, v: &SizeValue, resolved: f32) -> SizeValue {
        let n = v.numeric(resolved);
        match self {
            Self::Fixed => SizeValue::Fixed(n),
            Self::Hug => SizeValue::FitContent(None),
            Self::Fill => SizeValue::FillContainer(None),
        }
    }
}

impl fmt::Display for SizeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_schema())
    }
}

/// Formats a number for a field: at most 2 decimals, no trailing zeros, no
/// `-0`.
pub fn format_number(v: f32) -> String {
    format_with(v, 2)
}

pub fn format_with(v: f32, decimals: usize) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{v:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

/// Opacity 0..=1 as shown in the field ("50%").
pub fn format_percent(v: f32) -> String {
    format!("{}%", format_with(v * 100.0, 0))
}

/// Parses "50", "50%" or "0.5"-style input into 0..=1 (numbers > 1 are
/// percentages, like Pen/Figma).
pub fn parse_percent(input: &str, current: f32) -> Option<f32> {
    let t = input.trim();
    let t = t.strip_suffix('%').unwrap_or(t);
    let v = parse_number_input(t, current * 100.0)?;
    Some((v / 100.0).clamp(0.0, 1.0))
}

/// Parses a typed number. Supports `+ - * /`, parentheses and a leading
/// operator applied to `current` (e.g. `+10`, `*2`).
pub fn parse_number_input(input: &str, current: f32) -> Option<f32> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let expr = match t.as_bytes()[0] {
        b'*' | b'/' => format!("{}{t}", format_with(current, 6)),
        // "+10" / "-10" relative only when written with a space-free operator
        // and the remaining text is an expression: Figma treats "-10" as a
        // literal, so do we; only "+" is relative.
        b'+' => format!("{}{t}", format_with(current, 6)),
        _ => t.to_string(),
    };
    let v = eval(&expr)?;
    v.is_finite().then_some(v as f32)
}

/// Tiny recursive-descent evaluator for `+ - * / ( )` over f64.
fn eval(src: &str) -> Option<f64> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn peek(&mut self) -> Option<u8> {
            self.ws();
            self.s.get(self.i).copied()
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            while let Some(c @ (b'+' | b'-')) = self.peek() {
                self.i += 1;
                let r = self.term()?;
                v = if c == b'+' { v + r } else { v - r };
            }
            Some(v)
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.factor()?;
            while let Some(c @ (b'*' | b'/')) = self.peek() {
                self.i += 1;
                let r = self.factor()?;
                v = if c == b'*' { v * r } else { v / r };
            }
            Some(v)
        }
        fn factor(&mut self) -> Option<f64> {
            match self.peek()? {
                b'-' => {
                    self.i += 1;
                    Some(-self.factor()?)
                }
                b'+' => {
                    self.i += 1;
                    self.factor()
                }
                b'(' => {
                    self.i += 1;
                    let v = self.expr()?;
                    (self.peek()? == b')').then(|| self.i += 1)?;
                    Some(v)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len()
                        && (self.s[self.i].is_ascii_digit()
                            || self.s[self.i] == b'.'
                            || self.s[self.i] == b',')
                    {
                        self.i += 1;
                    }
                    let lit = std::str::from_utf8(&self.s[start..self.i])
                        .ok()?
                        .replace(',', ".");
                    lit.parse().ok()
                }
            }
        }
    }
    let mut p = P {
        s: src.as_bytes(),
        i: 0,
    };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

/// Modifiers that change scrub speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrubMods {
    /// Bigger steps (×10 by default, ×15 for rotation).
    pub shift: bool,
    /// Fine steps (×0.1), an OpenPen addition.
    pub alt: bool,
}

/// Range and granularity of a scrubbable field. Pen moves one `step` every
/// `px_per_step` pixels of horizontal drag (default: 1 unit per 2 px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrubSpec {
    pub px_per_step: f32,
    pub step: f32,
    pub shift_factor: f32,
    pub min: f32,
    pub max: f32,
}

impl ScrubSpec {
    pub const UNBOUNDED: Self = Self {
        px_per_step: 2.0,
        step: 1.0,
        shift_factor: 10.0,
        min: f32::MIN,
        max: f32::MAX,
    };
    pub const NON_NEGATIVE: Self = Self {
        min: 0.0,
        ..Self::UNBOUNDED
    };
    pub const ANGLE: Self = Self {
        shift_factor: 15.0,
        min: -360.0,
        max: 360.0,
        ..Self::UNBOUNDED
    };
    /// Opacity stored as 0..=1, scrubbed 1 % per step.
    pub const UNIT: Self = Self {
        step: 0.01,
        min: 0.0,
        max: 1.0,
        ..Self::UNBOUNDED
    };
}

/// Value after dragging `dx` pixels from where the scrub started. The value
/// moves in whole steps (anchored at `start`) so it stays round.
pub fn scrub_value(start: f32, dx: f32, mods: ScrubMods, spec: ScrubSpec) -> f32 {
    let factor = if mods.shift {
        spec.shift_factor
    } else if mods.alt {
        0.1
    } else {
        1.0
    };
    let step = spec.step * factor;
    let steps = (dx / spec.px_per_step.max(0.01)).trunc();
    let v = start + steps * step;
    // Kill float noise (13.000001) at a tenth of the fine step.
    let grain = (spec.step * 0.01).max(1e-5);
    ((v / grain).round() * grain).clamp(spec.min, spec.max)
}

/// The shared value of a multi-selection, or `None` when they differ
/// ("Mixed").
pub fn common<T: PartialEq + Clone>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.all(|v| v == first).then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trip() {
        for s in [
            "fill_container",
            "fit_content",
            "fill_container(120)",
            "fit_content(80.5)",
            "$width",
            "240",
            "0.25",
        ] {
            let v = SizeValue::parse_schema(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(v.to_schema(), s);
        }
        assert_eq!(
            SizeValue::parse_schema(" fill_container ( 10 ) "),
            Some(SizeValue::FillContainer(Some(10.0)))
        );
        assert_eq!(SizeValue::parse_schema("fill_container(abc)"), None);
        assert_eq!(SizeValue::parse_schema("fill_container(10"), None);
        assert_eq!(SizeValue::parse_schema("$"), None);
        assert_eq!(SizeValue::parse_schema("NaN"), None);
        assert_eq!(SizeValue::parse_schema("wide"), None);
    }

    #[test]
    fn user_input_keywords_and_math() {
        assert_eq!(
            SizeValue::parse_user("Fill", 10.0),
            Some(SizeValue::FillContainer(None))
        );
        assert_eq!(
            SizeValue::parse_user("hug", 10.0),
            Some(SizeValue::FitContent(None))
        );
        assert_eq!(
            SizeValue::parse_user("fill_container(30)", 10.0),
            Some(SizeValue::FillContainer(Some(30.0)))
        );
        assert_eq!(
            SizeValue::parse_user("100/4", 10.0),
            Some(SizeValue::Fixed(25.0))
        );
        assert_eq!(
            SizeValue::parse_user("*2", 12.0),
            Some(SizeValue::Fixed(24.0))
        );
        assert_eq!(
            SizeValue::parse_user("-5", 12.0),
            Some(SizeValue::Fixed(0.0)),
            "sizes clamp at 0"
        );
        assert_eq!(
            SizeValue::parse_user("$gap", 12.0),
            Some(SizeValue::Variable("gap".into()))
        );
        assert_eq!(SizeValue::parse_user("", 12.0), None);
        assert_eq!(SizeValue::parse_user("12px", 12.0), None);
    }

    #[test]
    fn mode_switch_keeps_number() {
        let v = SizeValue::FillContainer(None);
        assert_eq!(v.mode(), SizeMode::Fill);
        assert_eq!(SizeMode::Fixed.apply(&v, 320.0), SizeValue::Fixed(320.0));
        assert_eq!(
            SizeMode::Hug.apply(&SizeValue::Fixed(5.0), 5.0),
            SizeValue::FitContent(None)
        );
        assert_eq!(v.display(320.0), "320");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(12.0), "12");
        assert_eq!(format_number(12.5), "12.5");
        assert_eq!(format_number(12.345), "12.35");
        assert_eq!(format_number(-0.001), "0");
        assert_eq!(format_number(f32::NAN), "0");
        assert_eq!(format_number(1e6), "1000000");
        assert_eq!(format_percent(0.5), "50%");
        assert_eq!(format_percent(1.0), "100%");
    }

    #[test]
    fn number_parsing() {
        assert_eq!(parse_number_input("42", 0.0), Some(42.0));
        assert_eq!(parse_number_input(" 2 + 3 * 4 ", 0.0), Some(14.0));
        assert_eq!(parse_number_input("(2+3)*4", 0.0), Some(20.0));
        assert_eq!(parse_number_input("+10", 5.0), Some(15.0));
        assert_eq!(parse_number_input("-10", 5.0), Some(-10.0));
        assert_eq!(parse_number_input("/2", 5.0), Some(2.5));
        assert_eq!(parse_number_input("1,5", 0.0), Some(1.5));
        assert_eq!(parse_number_input("1/0", 0.0), None);
        assert_eq!(parse_number_input("2+", 0.0), None);
        assert_eq!(parse_number_input("abc", 0.0), None);
    }

    #[test]
    fn percent_parsing() {
        assert_eq!(parse_percent("50%", 1.0), Some(0.5));
        assert_eq!(parse_percent("50", 1.0), Some(0.5));
        assert_eq!(parse_percent("150", 1.0), Some(1.0));
        assert_eq!(parse_percent("-3", 1.0), Some(0.0));
        assert_eq!(parse_percent("+10", 0.5), Some(0.6));
    }

    #[test]
    fn scrub_speeds() {
        let s = ScrubSpec::UNBOUNDED;
        // 1 unit per 2 px.
        assert_eq!(scrub_value(10.0, 10.0, ScrubMods::default(), s), 15.0);
        assert_eq!(scrub_value(10.0, -25.0, ScrubMods::default(), s), -2.0);
        assert_eq!(
            scrub_value(
                13.0,
                2.0,
                ScrubMods {
                    shift: true,
                    alt: false
                },
                s
            ),
            23.0
        );
        let v = scrub_value(
            13.0,
            6.0,
            ScrubMods {
                shift: false,
                alt: true,
            },
            s,
        );
        assert!((v - 13.3).abs() < 1e-4, "{v}");
        // Less than one step of motion does nothing.
        assert_eq!(scrub_value(10.0, 1.9, ScrubMods::default(), s), 10.0);
        // Rotation: shift = 15 degrees.
        assert_eq!(
            scrub_value(
                0.0,
                4.0,
                ScrubMods {
                    shift: true,
                    alt: false
                },
                ScrubSpec::ANGLE
            ),
            30.0
        );
    }

    #[test]
    fn scrub_clamps() {
        assert_eq!(
            scrub_value(3.0, -20.0, ScrubMods::default(), ScrubSpec::NON_NEGATIVE),
            0.0
        );
        assert_eq!(
            scrub_value(0.95, 20.0, ScrubMods::default(), ScrubSpec::UNIT),
            1.0
        );
        let v = scrub_value(0.5, 14.0, ScrubMods::default(), ScrubSpec::UNIT);
        assert!((v - 0.57).abs() < 1e-4, "{v}");
        assert_eq!(
            scrub_value(350.0, 60.0, ScrubMods::default(), ScrubSpec::ANGLE),
            360.0
        );
    }

    #[test]
    fn mixed_values() {
        assert_eq!(common([1, 1, 1].into_iter()), Some(1));
        assert_eq!(common([1, 2].into_iter()), None);
        assert_eq!(common(std::iter::empty::<i32>()), None);
    }
}
