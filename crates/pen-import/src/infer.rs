//! Optional pass: turns simple CSS flex rows/columns back into `.pen` auto layout.
//!
//! A frame qualifies when it came from a non-wrapping `display: flex` element
//! and its children (all fixed-size, untransformed, in flow, no text nodes whose
//! measured width could change) sit in DOM order along the main axis with one
//! uniform gap and a common cross-axis alignment. The resulting layout is
//! simulated and only kept when every child lands within 1 px of its captured rect.

use serde_json::{Value, json};

use crate::convert::{Ir, round2};

/// (main pos, main size, cross pos, cross size) of a child, relative to the frame.
type Item = (f64, f64, f64, f64);

const TOL: f64 = 1.0;

pub(crate) fn infer(ir: &mut Ir) {
    for c in &mut ir.children {
        infer(c);
    }
    if let Some(layout) = try_layout(ir) {
        for (k, v) in layout {
            ir.props.insert(k.into(), v);
        }
    }
}

fn try_layout(ir: &Ir) -> Option<Vec<(&'static str, Value)>> {
    let flex = ir.flex.as_ref()?;
    if ir.ty != "frame" || ir.children.is_empty() {
        return None;
    }
    if ir
        .children
        .iter()
        .any(|c| c.ty == "text" || c.out_of_flow || c.rotation != 0.0 || c.shift != (0.0, 0.0))
    {
        return None;
    }
    let row = flex.row;
    let [fx, fy, fw, fh] = ir.rect;
    // (main pos, main size, cross pos, cross size) relative to the frame.
    let items: Vec<Item> = ir
        .children
        .iter()
        .map(|c| {
            let [x, y, w, h] = c.rect;
            if row {
                (x - fx, w, y - fy, h)
            } else {
                (y - fy, h, x - fx, w)
            }
        })
        .collect();
    let (main_len, cross_len) = if row { (fw, fh) } else { (fh, fw) };
    let pad_main = items[0].0;
    if pad_main < -TOL {
        return None;
    }
    let mut gap = 0.0;
    for (i, w) in items.windows(2).enumerate() {
        let g = w[1].0 - (w[0].0 + w[0].1);
        if i == 0 {
            gap = g;
        } else if (g - gap).abs() > TOL {
            return None;
        }
    }
    if gap < -TOL {
        return None;
    }
    let gap = gap.max(0.0);
    // Cross-axis alignment shared by every child.
    let same = |f: &dyn Fn(&Item) -> f64| {
        let v0 = f(&items[0]);
        items
            .iter()
            .all(|it| (f(it) - v0).abs() <= TOL)
            .then_some(v0)
    };
    let (align, pad_cs, pad_ce) = if let Some(start) = same(&|it| it.2) {
        ("start", start.max(0.0), 0.0)
    } else if let Some(center) = same(&|it| it.2 + it.3 / 2.0) {
        let s = (2.0 * center - cross_len).max(0.0);
        let e = (cross_len - 2.0 * center).max(0.0);
        ("center", s, e)
    } else {
        let end = same(&|it| it.2 + it.3)?;
        ("end", 0.0, (cross_len - end).max(0.0))
    };
    // Simulate and verify.
    let mut pos = pad_main;
    let avail = cross_len - pad_cs - pad_ce;
    for it in &items {
        let cross = match align {
            "start" => pad_cs,
            "center" => pad_cs + (avail - it.3) / 2.0,
            _ => pad_cs + avail - it.3,
        };
        if (pos - it.0).abs() > TOL || (cross - it.2).abs() > TOL {
            return None;
        }
        pos += it.1 + gap;
    }
    if pos - gap > main_len + TOL {
        return None;
    }
    let (t, r, b, l) = if row {
        (pad_cs, 0.0, pad_ce, pad_main)
    } else {
        (pad_main, pad_ce, 0.0, pad_cs)
    };
    let mut out = vec![
        (
            "layout",
            Value::String(if row { "horizontal" } else { "vertical" }.into()),
        ),
        ("alignItems", Value::String(align.into())),
    ];
    if gap > 0.0 {
        out.push(("gap", json!(round2(gap))));
    }
    if t > 0.0 || r > 0.0 || b > 0.0 || l > 0.0 {
        out.push((
            "padding",
            json!([round2(t), round2(r), round2(b), round2(l)]),
        ));
    }
    Some(out)
}
