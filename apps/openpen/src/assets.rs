//! Asset source: the Lucide catalog shipped with gpui-kit, re-stroked thin.
//!
//! Pen draws every icon with a 1 px non-scaling stroke (docs/ui-spec.md §5).
//! Lucide SVGs use `stroke-width="2"` on a 24 px grid; at the 14–16 px sizes
//! of the UI, 1.5 gives the same ~1 px hairline.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

pub struct PenAssets;

pub const ICON_STROKE: &str = "1.5";

pub fn thin_stroke(svg: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(svg).ok()?;
    s.contains("stroke-width=\"2\"").then(|| {
        s.replace(
            "stroke-width=\"2\"",
            &format!("stroke-width=\"{ICON_STROKE}\""),
        )
        .into_bytes()
    })
}

impl AssetSource for PenAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let inner = gpui_kit::assets::AllAssets.load(path)?;
        Ok(inner.map(|bytes| {
            match path
                .ends_with(".svg")
                .then(|| thin_stroke(&bytes))
                .flatten()
            {
                Some(v) => Cow::Owned(v),
                None => bytes,
            }
        }))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::AllAssets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrokes_lucide_svg() {
        let svg = br#"<svg stroke-width="2" viewBox="0 0 24 24"><path d="M0 0"/></svg>"#;
        let out = String::from_utf8(thin_stroke(svg).unwrap()).unwrap();
        assert!(out.contains(r#"stroke-width="1.5""#));
        assert!(thin_stroke(b"<svg/>").is_none());
    }

    #[test]
    fn loads_bundled_icon() {
        let icon = PenAssets
            .load(&gpui_kit::assets::IconName::Frame.path())
            .unwrap()
            .expect("frame icon");
        assert!(std::str::from_utf8(&icon).unwrap().contains("<svg"));
    }
}
