//! Shader animation scheduling (`@time` / `@mouse`), without a window.
//!
//! Like Pen (`animatingNodes` in the bundle), only shaders of selected nodes (or of
//! nodes inside a selected node) animate; the others keep their last frame. On top of
//! that, a node must be visible in the viewport and the window active: with nothing to
//! animate the canvas stops ticking (no idle CPU/GPU).
//!
//! Per node: `@time` is seconds accumulated while the node animates (Pen keeps a clock
//! per node; frame steps are capped at 0.1 s). While the previous frame is still rendering,
//! time accumulates and the next tick takes the whole step (Pen advances only once all
//! damage is shown). `@mouse` eases toward the pointer in node-local units
//! (`target + (cur - target) * exp(-5 dt)`, Pen's `c1(cur, target, 5, dt)`), starting at
//! the node center; it follows the pointer anywhere on the canvas, not only over the node.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use pen_layout::ShaderAnim;
use pen_render::{Affine2, NodeKey, Rect, Vec2};

/// Interval between animation ticks (caps the rate at ~60 fps).
pub const FRAME: Duration = Duration::from_millis(16);
/// A canvas that has not rendered for this long while animating is not being drawn
/// (minimized window): the animation pauses.
pub const STALL: Duration = Duration::from_millis(300);
/// Largest time step of one tick (Pen clamps its frame delta to 0.1 s).
pub const MAX_STEP: f32 = 0.1;
/// Easing rate of `@mouse` toward the pointer (1/s).
pub const MOUSE_RATE: f32 = 5.0;

/// A node with animated shaders (see `SceneDocument::shader_anim_candidates`).
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub key: NodeKey,
    /// World -> node-local transform.
    pub to_local: Affine2,
    pub size: Vec2,
    /// World bounds of the node box.
    pub aabb: Rect,
    /// The node or an ancestor is selected.
    pub selected: bool,
    pub time: bool,
    pub mouse: bool,
}

/// The candidates that animate now: selected (or inside a selected node) and
/// intersecting the visible part of the document. Nothing when the window is inactive.
pub fn active(cands: &[Candidate], visible: &Rect, window_active: bool) -> Vec<Candidate> {
    if !window_active {
        return Vec::new();
    }
    cands
        .iter()
        .filter(|c| c.selected && intersects(&c.aabb, visible))
        .copied()
        .collect()
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x <= b.right() && b.x <= a.right() && a.y <= b.bottom() && b.y <= a.bottom()
}

/// Per-node animation clocks.
#[derive(Debug, Default)]
pub struct ShaderAnimator {
    nodes: HashMap<NodeKey, ShaderAnim>,
    /// Time of the last tick (`None` = stopped: the next tick starts with no step).
    last: Option<Instant>,
    /// Time accumulated while the engine was busy.
    carry: f32,
}

impl ShaderAnimator {
    pub fn new() -> Self {
        Self::default()
    }

    /// True between the first tick after [`Self::stop`] and the next stop.
    pub fn running(&self) -> bool {
        self.last.is_some()
    }

    /// Stops the clocks (nothing animates): the next tick does not count the pause.
    pub fn stop(&mut self) {
        self.last = None;
        self.carry = 0.0;
    }

    /// Current state of a node.
    pub fn state(&self, k: NodeKey) -> Option<ShaderAnim> {
        self.nodes.get(&k).copied()
    }

    /// One tick at `now` for the `active` nodes. `pointer` is the pointer in world
    /// space, if known. While `busy` (the previous frame is still rendering) the step is
    /// carried over and nothing is returned; otherwise returns the new state of every
    /// active node.
    pub fn tick(
        &mut self,
        now: Instant,
        active: &[Candidate],
        pointer: Option<Vec2>,
        busy: bool,
    ) -> Vec<(NodeKey, ShaderAnim)> {
        let step = match self.last {
            Some(t) => now.saturating_duration_since(t).as_secs_f32().min(MAX_STEP),
            None => 0.0,
        };
        self.last = Some(now);
        if busy {
            self.carry += step;
            return Vec::new();
        }
        let dt = std::mem::take(&mut self.carry) + step;
        active
            .iter()
            .map(|c| {
                let center = c.size * 0.5;
                let s = self.nodes.entry(c.key).or_insert(ShaderAnim {
                    time: 0.0,
                    mouse: c.mouse.then_some(center),
                });
                if c.time {
                    s.time += dt;
                }
                if c.mouse {
                    let cur = s.mouse.unwrap_or(center);
                    s.mouse = Some(match pointer {
                        Some(p) => ease(cur, c.to_local.transform_point2(p), dt),
                        None => cur,
                    });
                }
                (c.key, *s)
            })
            .collect()
    }

    /// Forgets nodes that no longer have animated shaders.
    pub fn retain(&mut self, live: impl Fn(NodeKey) -> bool) {
        self.nodes.retain(|k, _| live(*k));
    }
}

/// Pen's `c1(cur, target, 5, dt)`, snapped once closer than 0.01 units (so a still pointer
/// stops producing new frames for mouse-only shaders).
fn ease(cur: Vec2, target: Vec2, dt: f32) -> Vec2 {
    let v = target + (cur - target) * (-MOUSE_RATE * dt).exp();
    if v.distance(target) < 0.01 { target } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(k: u32, x: f32, selected: bool) -> Candidate {
        Candidate {
            key: NodeKey(k),
            to_local: Affine2::from_translation(Vec2::new(-x, 0.0)),
            size: Vec2::new(100.0, 50.0),
            aabb: Rect::new(x, 0.0, 100.0, 50.0),
            selected,
            time: true,
            mouse: true,
        }
    }

    #[test]
    fn only_selected_visible_nodes_in_an_active_window() {
        let cands = [
            cand(1, 0.0, true),
            cand(2, 500.0, true),
            cand(3, 0.0, false),
        ];
        let view = Rect::new(-10.0, -10.0, 200.0, 200.0);
        let a = active(&cands, &view, true);
        assert_eq!(a.iter().map(|c| c.key.0).collect::<Vec<_>>(), vec![1]);
        // Scrolled away: nothing animates.
        assert!(active(&cands, &Rect::new(1000.0, 0.0, 100.0, 100.0), true).is_empty());
        // Window inactive / minimized: nothing animates.
        assert!(active(&cands, &view, false).is_empty());
    }

    #[test]
    fn time_accumulates_capped_and_carried_while_busy() {
        let mut a = ShaderAnimator::new();
        let c = [cand(1, 0.0, true)];
        let t0 = Instant::now();
        // First tick: no step yet.
        let f = a.tick(t0, &c, None, false);
        assert_eq!(f[0].1.time, 0.0);
        let f = a.tick(t0 + Duration::from_millis(16), &c, None, false);
        assert!((f[0].1.time - 0.016).abs() < 1e-4);
        // A long stall counts at most 0.1 s.
        let f = a.tick(t0 + Duration::from_secs(5), &c, None, false);
        assert!((f[0].1.time - 0.116).abs() < 1e-4);
        // Busy: carried over, applied on the next free tick.
        let t1 = t0 + Duration::from_secs(5);
        assert!(
            a.tick(t1 + Duration::from_millis(20), &c, None, true)
                .is_empty()
        );
        let f = a.tick(t1 + Duration::from_millis(40), &c, None, false);
        assert!((f[0].1.time - 0.156).abs() < 1e-4);
        // Stopped: the pause does not count.
        a.stop();
        assert!(!a.running());
        let f = a.tick(t1 + Duration::from_secs(60), &c, None, false);
        assert!((f[0].1.time - 0.156).abs() < 1e-4);
        assert!(a.running());
    }

    #[test]
    fn mouse_eases_toward_the_pointer_in_local_units() {
        let mut a = ShaderAnimator::new();
        let c = [cand(1, 200.0, true)];
        let t0 = Instant::now();
        let f = a.tick(t0, &c, Some(Vec2::new(210.0, 5.0)), false);
        // Starts at the center.
        assert_eq!(f[0].1.mouse, Some(Vec2::new(50.0, 25.0)));
        let f = a.tick(
            t0 + Duration::from_millis(100),
            &c,
            Some(Vec2::new(210.0, 5.0)),
            false,
        );
        let m = f[0].1.mouse.unwrap();
        // Local target (10, 5); one 0.1 s step covers 1 - e^-0.5 = 39 % of the way.
        let k = 1.0 - (-0.5f32).exp();
        assert!((m.x - (50.0 + (10.0 - 50.0) * k)).abs() < 1e-3);
        assert!((m.y - (25.0 + (5.0 - 25.0) * k)).abs() < 1e-3);
        // Converges and snaps (no endless tiny changes).
        let mut t = t0 + Duration::from_millis(100);
        let mut last = m;
        for _ in 0..100 {
            t += Duration::from_millis(100);
            last = a.tick(t, &c, Some(Vec2::new(210.0, 5.0)), false)[0]
                .1
                .mouse
                .unwrap();
        }
        assert_eq!(last, Vec2::new(10.0, 5.0));
    }

    #[test]
    fn time_only_shaders_keep_no_mouse() {
        let mut a = ShaderAnimator::new();
        let mut c = cand(1, 0.0, true);
        c.mouse = false;
        let f = a.tick(Instant::now(), &[c], Some(Vec2::new(1.0, 1.0)), false);
        assert_eq!(f[0].1.mouse, None);
    }
}
