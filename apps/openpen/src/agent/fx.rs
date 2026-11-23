//! Live feedback of agent edits on the canvas, as pure data + timing
//! functions (no GPUI): the "agent flash" (an ADSR ring plus a sweep line over
//! the bounds of every node a script touched) and the "agent cursor" (an arrow
//! with the agent's name that glides to the nodes being edited).
//!
//! Behaviour follows the legacy app (docs/legacy-analysis/04 §3.4); the code
//! is our own. Everything is a function of time, so the painter only asks
//! "what does it look like at `now`" and keeps requesting frames while
//! [`AgentFx::animating`] is true.

use std::time::{Duration, Instant};

use pen_render::{Rect, Vec2};

/// Flash envelope (seconds): ramp up, hold, ramp down.
pub const ATTACK: f32 = 0.2;
pub const HOLD: f32 = 0.4;
/// Hold for reads (`Get` and friends): the agent is "looking" for longer.
pub const HOLD_READ: f32 = 1.5;
pub const RELEASE: f32 = 0.3;

/// Sweep line speed in screen px per second (constant apparent speed).
pub const SWEEP_SPEED: f32 = 900.0;
/// Bounds of one sweep pass (seconds).
pub const SWEEP_MIN: f32 = 0.25;
pub const SWEEP_MAX: f32 = 1.2;
/// Sweep passes of a write flash; reads use `SWEEP_PASSES * 1.5`.
pub const SWEEP_PASSES: f32 = 3.0;

/// Cursor spring: exponential approach rate (1/s).
pub const SPRING_RATE: f32 = 8.0;
/// Cursor fade in / idle time before fading out / fade out (seconds).
pub const FADE_IN: f32 = 0.2;
pub const IDLE: f32 = 5.0;
pub const FADE_OUT: f32 = 1.5;
/// A flash waits this long when the cursor had to travel to its node.
pub const TRAVEL_DELAY: f32 = 0.3;
/// Delay between consecutive touched nodes of one call (shrinks for long
/// lists so a call never animates for more than [`MAX_SEQUENCE`]).
pub const STEP: f32 = 0.25;
pub const MAX_SEQUENCE: f32 = 2.5;
/// At most this many flashes per call (the rest would be noise).
pub const MAX_FLASHES: usize = 24;

/// Agent colors, assigned in connection order (RGB).
pub const AGENT_COLORS: [u32; 6] = [0x8b5cf6, 0xf97316, 0x10b981, 0xec4899, 0x0ea5e9, 0xeab308];

fn secs(d: Duration) -> f32 {
    d.as_secs_f32()
}

/// Seconds from `from` to `to`, negative when `to` is earlier.
pub fn since(from: Instant, to: Instant) -> f32 {
    if to >= from {
        secs(to - from)
    } else {
        -secs(from - to)
    }
}

/// ADSR-style envelope (attack, hold, release; sustain = 1) at `t` seconds
/// after the start: 0 before and after, 1 during the hold. Smoothstep ramps.
pub fn envelope(t: f32, hold: f32) -> f32 {
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    if t <= 0.0 {
        0.0
    } else if t < ATTACK {
        smooth(t / ATTACK)
    } else if t < ATTACK + hold {
        1.0
    } else if t < ATTACK + hold + RELEASE {
        1.0 - smooth((t - ATTACK - hold) / RELEASE)
    } else {
        0.0
    }
}

/// Length of one flash (seconds).
pub fn flash_duration(read: bool) -> f32 {
    ATTACK + if read { HOLD_READ } else { HOLD } + RELEASE
}

/// Duration of one sweep pass over a node `screen_h` px tall.
pub fn sweep_period(screen_h: f32) -> f32 {
    (screen_h.max(0.0) / SWEEP_SPEED).clamp(SWEEP_MIN, SWEEP_MAX)
}

/// Position of the sweep line (0 = top, 1 = bottom) at `t` seconds into a
/// flash, or `None` once its passes are done (or before it starts).
pub fn sweep(t: f32, screen_h: f32, read: bool) -> Option<f32> {
    if t <= 0.0 {
        return None;
    }
    let period = sweep_period(screen_h);
    let passes = if read {
        SWEEP_PASSES * 1.5
    } else {
        SWEEP_PASSES
    };
    // Never outlive the flash itself.
    let end = (period * passes).min(flash_duration(read));
    if t >= end {
        return None;
    }
    let x = (t / period).fract();
    // Ease in-out inside each pass.
    Some(x * x * (3.0 - 2.0 * x))
}

/// Exponential spring: where a point that was at `from` and is pulled
/// toward `to` is after `dt` seconds.
pub fn spring(from: Vec2, to: Vec2, dt: f32) -> Vec2 {
    if dt <= 0.0 {
        return from;
    }
    let k = (-SPRING_RATE * dt).exp();
    to + (from - to) * k
}

/// Cursor opacity: fade in after `visible_since`, fade out after `IDLE`
/// seconds without a new target.
pub fn cursor_opacity(since_shown: f32, since_active: f32) -> f32 {
    let fade_in = (since_shown / FADE_IN).clamp(0.0, 1.0);
    let fade_out = if since_active <= IDLE {
        1.0
    } else {
        1.0 - ((since_active - IDLE) / FADE_OUT).clamp(0.0, 1.0)
    };
    fade_in.min(fade_out)
}

/// Small deterministic RNG (xorshift) for cursor targets.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Triangular distribution on `[0, 1]` peaking at 0.5.
    pub fn triangular(&mut self) -> f32 {
        (self.next_f32() + self.next_f32()) / 2.0
    }
}

/// A point inside `r` (triangular in both axes, so cursors gather near the
/// middle without stacking exactly on it).
pub fn target_in(r: &Rect, rng: &mut Rng) -> Vec2 {
    // Keep a margin so the arrow tip is visibly inside small nodes too.
    let (u, v) = (0.2 + 0.6 * rng.triangular(), 0.2 + 0.6 * rng.triangular());
    Vec2::new(r.x + r.w * u, r.y + r.h * v)
}

/// One flash over one node.
#[derive(Debug, Clone)]
pub struct Flash {
    /// Node id path (re-resolved at paint time so the ring follows layout).
    pub id: String,
    /// World bounds when the flash was created (fallback).
    pub rect: Rect,
    pub color: u32,
    pub start: Instant,
    pub read: bool,
}

impl Flash {
    pub fn end(&self) -> Instant {
        self.start + Duration::from_secs_f32(flash_duration(self.read))
    }

    /// Envelope at `now`.
    pub fn alpha(&self, now: Instant) -> f32 {
        let hold = if self.read { HOLD_READ } else { HOLD };
        envelope(since(self.start, now), hold)
    }

    /// Sweep position at `now` for a node `screen_h` px tall.
    pub fn sweep(&self, now: Instant, screen_h: f32) -> Option<f32> {
        sweep(since(self.start, now), screen_h, self.read)
    }
}

/// One agent's cursor: a piecewise spring through timed waypoints.
#[derive(Debug, Clone)]
pub struct AgentCursor {
    pub client_id: String,
    pub name: String,
    pub color: u32,
    /// Position at `base_t` (all waypoints before it are folded in).
    base: Vec2,
    base_t: Instant,
    /// Target in effect from `base_t` until the first waypoint.
    base_target: Vec2,
    /// Future target changes, in time order.
    waypoints: Vec<(Instant, Vec2)>,
    shown: Instant,
    /// Time of the last waypoint (activity).
    active: Instant,
}

impl AgentCursor {
    pub fn new(client_id: &str, name: &str, color: u32, at: Vec2, now: Instant) -> Self {
        Self {
            client_id: client_id.into(),
            name: name.into(),
            color,
            base: at,
            base_t: now,
            base_target: at,
            waypoints: Vec::new(),
            shown: now,
            active: now,
        }
    }

    /// World position at `now`.
    pub fn position(&self, now: Instant) -> Vec2 {
        let mut p = self.base;
        let mut t = self.base_t;
        let mut target = self.base_target;
        for &(wt, wp) in &self.waypoints {
            if wt >= now {
                break;
            }
            p = spring(p, target, since(t, wt));
            t = wt;
            target = wp;
        }
        spring(p, target, since(t, now).max(0.0))
    }

    /// Folds waypoints that are already in the past into the base state.
    fn settle(&mut self, now: Instant) {
        while let Some(&(wt, wp)) = self.waypoints.first() {
            if wt > now {
                break;
            }
            self.base = spring(self.base, self.base_target, since(self.base_t, wt));
            self.base_t = wt;
            self.base_target = wp;
            self.waypoints.remove(0);
        }
    }

    /// Pulls the cursor toward `target` from time `at` on (`at >= now`).
    pub fn move_to(&mut self, target: Vec2, at: Instant, now: Instant) {
        // A cursor that already faded out reappears where it is.
        if now >= self.end() {
            self.base = self.position(now);
            self.base_t = now;
            self.base_target = self.base;
            self.waypoints.clear();
            self.shown = now;
        }
        self.settle(now);
        let i = self.waypoints.partition_point(|w| w.0 <= at);
        self.waypoints.insert(i, (at, target));
        if at > self.active {
            self.active = at;
        }
    }

    /// Where the cursor is headed at `at` (the last target set before it).
    pub fn target_at(&self, at: Instant) -> Vec2 {
        self.waypoints
            .iter()
            .take_while(|w| w.0 <= at)
            .last()
            .map(|w| w.1)
            .unwrap_or(self.base_target)
    }

    pub fn opacity(&self, now: Instant) -> f32 {
        cursor_opacity(since(self.shown, now), since(self.active, now))
    }

    /// Time after which the cursor is invisible.
    pub fn end(&self) -> Instant {
        self.active + Duration::from_secs_f32(IDLE + FADE_OUT)
    }

    /// Idle wobble in screen px (grows once the cursor has settled).
    pub fn wobble(&self, now: Instant) -> Vec2 {
        let idle = since(self.active, now);
        let amp = (idle / 0.6).clamp(0.0, 1.0) * 1.5;
        let t = since(self.shown, now);
        Vec2::new((t * 2.1).sin() * amp, (t * 1.7 + 0.8).cos() * amp)
    }
}

/// A node an agent call touched, resolved to world bounds.
#[derive(Debug, Clone)]
pub struct TouchedNode {
    pub id: String,
    pub rect: Rect,
    pub write: bool,
}

/// Flashes and cursors of one document.
#[derive(Debug, Clone)]
pub struct AgentFx {
    pub flashes: Vec<Flash>,
    pub cursors: Vec<AgentCursor>,
    rng: Rng,
}

impl Default for AgentFx {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentFx {
    pub fn new() -> Self {
        Self {
            flashes: Vec::new(),
            cursors: Vec::new(),
            rng: Rng::new(0x9e37_79b9_7f4a_7c15),
        }
    }

    /// Records the nodes one agent call touched: the agent's cursor visits
    /// them in order and each one flashes when the cursor gets there.
    /// `spawn` is where a new cursor appears (e.g. the viewport center).
    pub fn touch(
        &mut self,
        client_id: &str,
        name: &str,
        color: u32,
        nodes: &[TouchedNode],
        spawn: Vec2,
        now: Instant,
    ) {
        self.prune(now);
        if nodes.is_empty() {
            return;
        }
        let nodes = &nodes[..nodes.len().min(MAX_FLASHES)];
        let ci = match self.cursors.iter().position(|c| c.client_id == client_id) {
            Some(i) => i,
            None => {
                self.cursors
                    .push(AgentCursor::new(client_id, name, color, spawn, now));
                self.cursors.len() - 1
            }
        };
        // Start after whatever this cursor is still doing.
        let busy = self.cursors[ci]
            .waypoints
            .last()
            .map(|w| w.0)
            .filter(|&t| t > now)
            .unwrap_or(now);
        let step = STEP.min(MAX_SEQUENCE / nodes.len() as f32);
        for (i, n) in nodes.iter().enumerate() {
            let at = busy + Duration::from_secs_f32(step * i as f32);
            let target = target_in(&n.rect, &mut self.rng);
            let cur = &mut self.cursors[ci];
            let from = cur.target_at(at);
            let travelled = (from - target).length() > 1.0;
            cur.move_to(target, at, now);
            let delay = if travelled { TRAVEL_DELAY } else { 0.0 };
            self.flashes.push(Flash {
                id: n.id.clone(),
                rect: n.rect,
                color,
                start: at + Duration::from_secs_f32(delay),
                read: !n.write,
            });
        }
        let cur = &mut self.cursors[ci];
        cur.name = name.into();
        cur.color = color;
    }

    /// Drops finished flashes and faded cursors.
    pub fn prune(&mut self, now: Instant) {
        self.flashes.retain(|f| f.end() > now);
        self.cursors.retain(|c| c.end() > now);
    }

    /// Removes the cursor of an agent that disconnected.
    pub fn forget(&mut self, client_id: &str) {
        self.cursors.retain(|c| c.client_id != client_id);
    }

    /// Something is still moving or fading at `now` (keep requesting frames).
    pub fn animating(&self, now: Instant) -> bool {
        self.flashes.iter().any(|f| f.end() > now) || self.cursors.iter().any(|c| c.end() > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: f32) -> Instant {
        t0 + Duration::from_secs_f32(s)
    }

    #[test]
    fn envelope_shape() {
        assert_eq!(envelope(-0.1, HOLD), 0.0);
        assert_eq!(envelope(0.0, HOLD), 0.0);
        let mid_attack = envelope(ATTACK / 2.0, HOLD);
        assert!((mid_attack - 0.5).abs() < 1e-4);
        assert_eq!(envelope(ATTACK + 0.01, HOLD), 1.0);
        assert_eq!(envelope(ATTACK + HOLD - 0.01, HOLD), 1.0);
        let r = envelope(ATTACK + HOLD + RELEASE / 2.0, HOLD);
        assert!((r - 0.5).abs() < 1e-4);
        assert_eq!(envelope(ATTACK + HOLD + RELEASE + 0.01, HOLD), 0.0);
        // Reads hold longer.
        assert_eq!(envelope(ATTACK + 1.0, HOLD_READ), 1.0);
        assert_eq!(envelope(ATTACK + 1.0, HOLD), 0.0);
        assert!((flash_duration(false) - 0.9).abs() < 1e-6);
        assert!((flash_duration(true) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn sweep_speed_and_passes() {
        // Constant apparent speed: taller nodes take longer, within bounds.
        assert_eq!(sweep_period(90.0), SWEEP_MIN);
        assert!((sweep_period(450.0) - 0.5).abs() < 1e-6);
        assert_eq!(sweep_period(10_000.0), SWEEP_MAX);
        // 450 px: 0.5 s per pass, the flash (0.9 s) ends before 3 passes.
        assert_eq!(sweep(0.0, 450.0, false), None);
        let a = sweep(0.25, 450.0, false).unwrap();
        assert!((a - 0.5).abs() < 1e-4);
        assert!(sweep(0.89, 450.0, false).is_some());
        assert_eq!(sweep(0.95, 450.0, false), None);
        // Small node: 3 passes of 0.25 s, then done although the flash lasts.
        assert!(sweep(0.7, 50.0, false).is_some());
        assert_eq!(sweep(0.76, 50.0, false), None);
        // Reads sweep 1.5x as many times.
        assert!(sweep(1.1, 50.0, true).is_some());
        assert_eq!(sweep(1.13, 50.0, true), None);
    }

    #[test]
    fn spring_converges_monotonically() {
        let a = Vec2::ZERO;
        let b = Vec2::new(100.0, 0.0);
        assert_eq!(spring(a, b, 0.0), a);
        let mut last = 0.0;
        for i in 1..20 {
            let p = spring(a, b, i as f32 * 0.05);
            assert!(p.x > last && p.x < 100.0);
            last = p.x;
        }
        assert!((spring(a, b, 2.0).x - 100.0).abs() < 0.01);
        // Splitting the interval gives the same point (pure exponential).
        let half = spring(a, b, 0.1);
        let two = spring(half, b, 0.1);
        assert!((two - spring(a, b, 0.2)).length() < 1e-3);
    }

    #[test]
    fn cursor_opacity_fades() {
        assert_eq!(cursor_opacity(0.0, 0.0), 0.0);
        assert!((cursor_opacity(0.1, 0.1) - 0.5).abs() < 1e-5);
        assert_eq!(cursor_opacity(1.0, 1.0), 1.0);
        assert_eq!(cursor_opacity(10.0, IDLE), 1.0);
        assert!((cursor_opacity(10.0, IDLE + FADE_OUT / 2.0) - 0.5).abs() < 1e-5);
        assert_eq!(cursor_opacity(10.0, IDLE + FADE_OUT), 0.0);
    }

    #[test]
    fn rng_triangular_in_range() {
        let mut r = Rng::new(7);
        let mut sum = 0.0;
        for _ in 0..2000 {
            let v = r.triangular();
            assert!((0.0..=1.0).contains(&v));
            sum += v;
        }
        assert!((sum / 2000.0 - 0.5).abs() < 0.03);
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        for _ in 0..100 {
            let p = target_in(&rect, &mut r);
            assert!(p.x >= 30.0 && p.x <= 90.0 && p.y >= 30.0 && p.y <= 60.0);
        }
    }

    #[test]
    fn cursor_follows_waypoints() {
        let t0 = Instant::now();
        let mut c = AgentCursor::new("c1", "claude", 0, Vec2::ZERO, t0);
        c.move_to(Vec2::new(100.0, 0.0), t0, t0);
        c.move_to(Vec2::new(100.0, 100.0), at(t0, 1.0), t0);
        // Heading to the first target, then the second.
        assert!(c.position(at(t0, 0.2)).x > 50.0);
        assert!(c.position(at(t0, 0.9)).y.abs() < 1e-3);
        let p = c.position(at(t0, 3.0));
        assert!((p - Vec2::new(100.0, 100.0)).length() < 0.1);
        assert_eq!(c.target_at(at(t0, 0.5)), Vec2::new(100.0, 0.0));
        assert_eq!(c.target_at(at(t0, 2.0)), Vec2::new(100.0, 100.0));
        // Settling past waypoints does not move the cursor.
        let before = c.position(at(t0, 1.5));
        c.settle(at(t0, 1.2));
        assert!((c.position(at(t0, 1.5)) - before).length() < 1e-3);
        // Visible, then fades after the idle time.
        assert_eq!(c.opacity(at(t0, 2.0)), 1.0);
        assert_eq!(c.opacity(at(t0, 1.0 + IDLE + FADE_OUT + 0.1)), 0.0);
    }

    #[test]
    fn fx_sequences_flashes_and_stops_animating() {
        let t0 = Instant::now();
        let mut fx = AgentFx::new();
        assert!(!fx.animating(t0));
        let nodes: Vec<TouchedNode> = (0..3)
            .map(|i| TouchedNode {
                id: format!("n{i}"),
                rect: Rect::new(i as f32 * 200.0, 0.0, 100.0, 100.0),
                write: true,
            })
            .collect();
        fx.touch("c1", "claude", 0x8b5cf6, &nodes, Vec2::new(-500.0, 0.0), t0);
        assert_eq!(fx.flashes.len(), 3);
        assert_eq!(fx.cursors.len(), 1);
        // Staggered, each delayed by the cursor's travel.
        let starts: Vec<f32> = fx.flashes.iter().map(|f| since(t0, f.start)).collect();
        assert!((starts[0] - TRAVEL_DELAY).abs() < 1e-3);
        assert!((starts[1] - STEP - TRAVEL_DELAY).abs() < 1e-3);
        assert!(starts[2] > starts[1]);
        assert!(fx.animating(at(t0, 1.0)));
        // The cursor ends inside the last node.
        let p = fx.cursors[0].position(at(t0, 3.0));
        assert!(p.x >= 400.0 && p.x <= 500.0 && p.y >= 0.0 && p.y <= 100.0);
        // A second call of the same agent reuses its cursor and queues after.
        fx.touch(
            "c1",
            "claude",
            0x8b5cf6,
            &nodes[..1],
            Vec2::ZERO,
            at(t0, 0.1),
        );
        assert_eq!(fx.cursors.len(), 1);
        assert!(since(t0, fx.flashes[3].start) >= 2.0 * STEP);
        // Everything is over after the idle fade.
        let end = at(t0, 1.0 + IDLE + FADE_OUT + 0.1);
        assert!(!fx.animating(end));
        fx.prune(end);
        assert!(fx.flashes.is_empty() && fx.cursors.is_empty());
    }

    #[test]
    fn fx_caps_flashes_and_sequence_length() {
        let t0 = Instant::now();
        let mut fx = AgentFx::new();
        let nodes: Vec<TouchedNode> = (0..100)
            .map(|i| TouchedNode {
                id: format!("n{i}"),
                rect: Rect::new(i as f32 * 10.0, 0.0, 5.0, 5.0),
                write: i % 2 == 0,
            })
            .collect();
        fx.touch("c1", "a", 0, &nodes, Vec2::ZERO, t0);
        assert_eq!(fx.flashes.len(), MAX_FLASHES);
        let last = fx
            .flashes
            .iter()
            .map(|f| since(t0, f.start))
            .fold(0.0, f32::max);
        assert!(last <= MAX_SEQUENCE + TRAVEL_DELAY + 1e-3);
        assert!(fx.flashes.iter().any(|f| f.read));
        fx.forget("c1");
        assert!(fx.cursors.is_empty());
    }
}
