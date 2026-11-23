//! Pure helpers for the canvas overlay: frame-name truncation and the Alt
//! distance measurements (docs/ui-spec.md §3, legacy 02 §7.2).

use pen_render::{Rect, Vec2};

/// Frame names narrower than this (screen px) are hidden instead of being
/// squeezed into an ellipsis.
pub const LABEL_MIN_WIDTH: f32 = 24.0;

pub const ELLIPSIS: &str = "…";

/// Fits `text` into `max` screen px: the full text when it fits, otherwise
/// the longest prefix followed by an ellipsis; `None` when not even a
/// one-character prefix plus the ellipsis fits (or `max` is below
/// [`LABEL_MIN_WIDTH`]). `measure` returns the width of a string.
pub fn truncate_label(text: &str, max: f32, measure: impl Fn(&str) -> f32) -> Option<String> {
    if max < LABEL_MIN_WIDTH || text.is_empty() {
        return None;
    }
    if measure(text) <= max {
        return Some(text.to_string());
    }
    // Char boundaries; binary search the longest prefix that fits.
    let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).skip(1).collect();
    let fits = |n: usize| -> bool {
        let end = bounds.get(n.wrapping_sub(1)).copied().unwrap_or(0);
        let s = format!("{}{ELLIPSIS}", text[..end].trim_end());
        measure(&s) <= max
    };
    // n = number of chars kept (1..=bounds.len()).
    let (mut lo, mut hi) = (0usize, bounds.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return None;
    }
    Some(format!("{}{ELLIPSIS}", text[..bounds[lo - 1]].trim_end()))
}

/// One distance line of an Alt measurement (world space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    pub a: Vec2,
    pub b: Vec2,
    /// Distance in document units.
    pub value: f32,
}

/// Distances between the selection box `sel` and the hovered node `other`,
/// like Pen's Alt-hover: when one contains the other, the four inner gaps;
/// otherwise the horizontal and/or vertical gap between them.
pub fn measure(sel: &Rect, other: &Rect) -> Vec<Measure> {
    let mut out = Vec::new();
    let contains = |o: &Rect, i: &Rect| {
        o.x <= i.x && o.y <= i.y && o.right() >= i.right() && o.bottom() >= i.bottom()
    };
    let (inner, outer) = if contains(other, sel) {
        (Some(*sel), Some(*other))
    } else if contains(sel, other) {
        (Some(*other), Some(*sel))
    } else {
        (None, None)
    };
    if let (Some(i), Some(o)) = (inner, outer) {
        let cy = i.y + i.h / 2.0;
        let cx = i.x + i.w / 2.0;
        let mut push = |a: Vec2, b: Vec2| {
            let value = (b - a).length();
            if value > 0.0 {
                out.push(Measure { a, b, value });
            }
        };
        push(Vec2::new(o.x, cy), Vec2::new(i.x, cy));
        push(Vec2::new(i.right(), cy), Vec2::new(o.right(), cy));
        push(Vec2::new(cx, o.y), Vec2::new(cx, i.y));
        push(Vec2::new(cx, i.bottom()), Vec2::new(cx, o.bottom()));
        return out;
    }
    // Horizontal gap.
    let (l, r) = if sel.right() <= other.x {
        (Some(sel.right()), Some(other.x))
    } else if other.right() <= sel.x {
        (Some(other.right()), Some(sel.x))
    } else {
        (None, None)
    };
    if let (Some(l), Some(r)) = (l, r) {
        // Middle of the vertical overlap, or the selection's center.
        let top = sel.y.max(other.y);
        let bottom = sel.bottom().min(other.bottom());
        let y = if top <= bottom {
            (top + bottom) / 2.0
        } else {
            sel.y + sel.h / 2.0
        };
        out.push(Measure {
            a: Vec2::new(l, y),
            b: Vec2::new(r, y),
            value: r - l,
        });
    }
    let (t, b) = if sel.bottom() <= other.y {
        (Some(sel.bottom()), Some(other.y))
    } else if other.bottom() <= sel.y {
        (Some(other.bottom()), Some(sel.y))
    } else {
        (None, None)
    };
    if let (Some(t), Some(b)) = (t, b) {
        let left = sel.x.max(other.x);
        let right = sel.right().min(other.right());
        let x = if left <= right {
            (left + right) / 2.0
        } else {
            sel.x + sel.w / 2.0
        };
        out.push(Measure {
            a: Vec2::new(x, t),
            b: Vec2::new(x, b),
            value: b - t,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 6 px per char, ellipsis included.
    fn m(s: &str) -> f32 {
        s.chars().count() as f32 * 6.0
    }

    #[test]
    fn label_fits_or_truncates() {
        assert_eq!(truncate_label("Home", 100.0, m).as_deref(), Some("Home"));
        // 30 px = 5 chars: 4 + ellipsis.
        assert_eq!(
            truncate_label("Settings page", 30.0, m).as_deref(),
            Some("Sett…")
        );
        // Trailing spaces before the ellipsis are trimmed.
        assert_eq!(truncate_label("Ab cdef", 24.0, m).as_deref(), Some("Ab…"));
    }

    #[test]
    fn label_hidden_when_too_narrow() {
        assert_eq!(truncate_label("Settings", 20.0, m), None);
        assert_eq!(truncate_label("", 200.0, m), None);
        // Wider than the minimum but not even "S…" fits with a wide font.
        assert_eq!(truncate_label("Settings", 25.0, |s| m(s) * 3.0), None);
    }

    #[test]
    fn label_respects_char_boundaries() {
        let out = truncate_label("Configurações — templates", 60.0, m).unwrap();
        assert!(out.ends_with(ELLIPSIS));
        assert!(m(&out) <= 60.0);
        assert_eq!(out, "Configura…");
    }

    #[test]
    fn measure_gaps_between_separate_nodes() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(30.0, 5.0, 10.0, 10.0);
        let v = measure(&a, &b);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].value, 20.0);
        assert_eq!(v[0].a, Vec2::new(10.0, 7.5));
        // Diagonal: both gaps.
        let c = Rect::new(30.0, 40.0, 5.0, 5.0);
        let v = measure(&a, &c);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].value, 20.0);
        assert_eq!(v[1].value, 30.0);
    }

    #[test]
    fn measure_inside_parent() {
        let parent = Rect::new(0.0, 0.0, 100.0, 50.0);
        let child = Rect::new(10.0, 20.0, 30.0, 10.0);
        let v = measure(&child, &parent);
        let values: Vec<f32> = v.iter().map(|m| m.value).collect();
        assert_eq!(values, vec![10.0, 60.0, 20.0, 20.0]);
        // Same result with the roles swapped.
        assert_eq!(measure(&parent, &child), v);
    }
}
