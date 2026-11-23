//! Zoom steps and labels for the zoom control (no GPUI). Pen jumps to the
//! next/previous power of two and clamps to 0.02–256 (docs/ui-spec.md §10).

pub const MIN_ZOOM: f32 = 0.02;
pub const MAX_ZOOM: f32 = 256.0;

pub fn clamp(z: f32) -> f32 {
    if z.is_finite() && z > 0.0 {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// `2^(floor(log2 z) + 1)`
pub fn zoom_in(z: f32) -> f32 {
    clamp(2f32.powf(clamp(z).log2().floor() + 1.0))
}

/// `2^(ceil(log2 z) - 1)`
pub fn zoom_out(z: f32) -> f32 {
    clamp(2f32.powf(clamp(z).log2().ceil() - 1.0))
}

/// `round(zoom × 100)%`
pub fn label(z: f32) -> String {
    format!("{}%", (clamp(z) * 100.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powers_of_two() {
        assert_eq!(zoom_in(1.0), 2.0);
        assert_eq!(zoom_out(1.0), 0.5);
        assert_eq!(zoom_in(0.75), 1.0);
        assert_eq!(zoom_out(0.75), 0.5);
        assert_eq!(zoom_in(3.0), 4.0);
        assert_eq!(zoom_out(3.0), 2.0);
    }

    #[test]
    fn bounded() {
        let mut z = 1.0;
        for _ in 0..20 {
            z = zoom_in(z);
        }
        assert_eq!(z, MAX_ZOOM);
        for _ in 0..20 {
            z = zoom_out(z);
        }
        assert_eq!(z, MIN_ZOOM);
        assert_eq!(clamp(f32::NAN), 1.0);
    }

    #[test]
    fn labels() {
        assert_eq!(label(1.0), "100%");
        assert_eq!(label(0.125), "13%");
        assert_eq!(label(2.0), "200%");
        assert_eq!(label(0.02), "2%");
    }
}
