//! Follow-camera: One Euro filtering plus a critically damped spring, with a
//! speed-driven viewport expansion. Ported 1:1 from the C++ reference so the
//! rendered result matches frame for frame.

use crate::settings::{aspect_value, Settings};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x as f32
            && p.y >= self.y as f32
            && p.x < self.right() as f32
            && p.y < self.bottom() as f32
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CamSnapshot {
    pub cursor: Vec2,
    pub center: Vec2,
    pub src_w: f32,
    pub src_h: f32,
    pub speed: f32,
    pub expand: f32,
    pub valid: bool,
    pub tracking: bool,
    /// The screen was too small for the requested viewport, so it was shrunk.
    pub limited: bool,
}

/// The viewport a Take asks for, and the one a monitor can actually show.
///
/// A viewport wider or taller than the screen would have to show empty space, so
/// the camera shrinks it to fit - which silently raises the effective zoom. On a
/// 1080-wide screen every zoom below 1.0 collapses to 1.0 and the speed-driven
/// expansion disappears, so the numbers are reported instead of applied quietly.
pub fn requested_viewport(s: &Settings) -> (f32, f32) {
    let out_w = s.out_width as f32;
    let src_w = out_w / s.zoom.clamp(0.1, 8.0);
    let aspect = aspect_value(s.aspect_idx);
    (src_w, src_w / aspect)
}

/// What a given monitor can actually show for these settings.
pub fn fit_viewport(s: &Settings, mon: IRect) -> (f32, f32, bool) {
    let (mut w, mut h) = requested_viewport(s);
    let mut fit = 1.0f32;
    if w > mon.w as f32 {
        fit = mon.w as f32 / w;
    }
    if h * fit > mon.h as f32 {
        fit *= mon.h as f32 / (h * fit);
    }
    let limited = fit < 1.0;
    if limited {
        w *= fit;
        h *= fit;
    }
    (w, h, limited)
}

/// How much the viewport is currently grown by speed, relative to its resting
/// size at this zoom. Derived from the rect itself so the live recorder and the
/// export bake the same number.
pub fn growth_factor(src_w: f32, out_w: i32, zoom: f32) -> f32 {
    let rest = out_w as f32 / zoom.clamp(0.1, 8.0);
    if rest <= 0.0 {
        return 1.0;
    }
    (src_w / rest).clamp(1.0, 4.0)
}

/// How fast the camera anchor follows the pointer.
///
/// Long on purpose: a hand circling in a small area has a centre that only shows
/// up once the loop averages out. Measured on a synthetic 1.6 Hz circle, 3 s puts
/// the anchor within 16 px of the true centre; 0.9 s leaves 38 px and a visible
/// drift. Going longer costs nothing on ordinary movement because the dead zone
/// already handles the transient.
const ANCHOR_SECS: f32 = 3.0;

fn clampf(v: f32, a: f32, b: f32) -> f32 {
    if v < a {
        a
    } else if v > b {
        b
    } else {
        v
    }
}

#[inline]
fn smooth_step(t: f32) -> f32 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One Euro filter (Casiez et al. 2012).
#[derive(Default)]
struct OneEuro {
    init: bool,
    x_prev: Vec2,
    dx_prev: Vec2,
}

impl OneEuro {
    fn reset(&mut self, p: Vec2) {
        self.init = true;
        self.x_prev = p;
        self.dx_prev = Vec2::default();
    }

    #[inline]
    fn alpha(cutoff: f32, dt: f64) -> f32 {
        if cutoff <= 0.0 {
            return 1.0;
        }
        let tau = 1.0 / (2.0 * std::f64::consts::PI * cutoff as f64);
        (1.0 / (1.0 + tau / dt)) as f32
    }

    fn filter(&mut self, x: Vec2, dt: f64, min_cutoff: f32, beta: f32) -> Vec2 {
        if !self.init {
            self.reset(x);
            return x;
        }
        if dt <= 1e-6 {
            return self.x_prev;
        }

        // Divide, do not multiply by the reciprocal: `a / dt` and `a * (1/dt)`
        // round differently and would drift the baked path by a fraction of a px.
        let dtf = dt as f32;
        let dx = Vec2::new(
            (x.x - self.x_prev.x) / dtf,
            (x.y - self.x_prev.y) / dtf,
        );
        let a_d = Self::alpha(1.0, dt);
        self.dx_prev.x += a_d * (dx.x - self.dx_prev.x);
        self.dx_prev.y += a_d * (dx.y - self.dx_prev.y);

        let spd = (self.dx_prev.x * self.dx_prev.x + self.dx_prev.y * self.dx_prev.y).sqrt();
        let a = Self::alpha(min_cutoff + beta * spd, dt);
        self.x_prev.x += a * (x.x - self.x_prev.x);
        self.x_prev.y += a * (x.y - self.x_prev.y);
        self.x_prev
    }
}

pub struct Camera {
    screen: IRect,
    center: Vec2,
    /// Where the camera would like to sit. It trails the pointer with a long time
    /// constant, so a pointing gesture that stays in one area converges onto the
    /// centre of that area instead of orbiting with the hand.
    anchor: Vec2,
    vel: Vec2,
    prev_filtered: Vec2,
    vel_smooth: Vec2,
    expand: f32,
    init: bool,
    euro: OneEuro,
    snap: CamSnapshot,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

impl Camera {
    pub fn new() -> Self {
        Self {
            screen: IRect::new(0, 0, 1920, 1080),
            center: Vec2::default(),
            anchor: Vec2::default(),
            vel: Vec2::default(),
            prev_filtered: Vec2::default(),
            vel_smooth: Vec2::default(),
            expand: 0.0,
            init: false,
            euro: OneEuro::default(),
            snap: CamSnapshot::default(),
        }
    }

    pub fn set_screen(&mut self, r: IRect) {
        if r != self.screen {
            self.screen = r;
            self.init = false;
        }
    }

    pub fn reset(&mut self, p: Vec2) {
        self.center = p;
        self.anchor = p;
        self.vel = Vec2::default();
        self.prev_filtered = p;
        self.vel_smooth = Vec2::default();
        self.expand = 0.0;
        self.euro.reset(p);
        self.init = true;
        self.snap = CamSnapshot {
            cursor: p,
            center: p,
            src_w: 1280.0,
            src_h: 720.0,
            valid: true,
            ..Default::default()
        };
    }

    pub fn snapshot(&self) -> CamSnapshot {
        self.snap
    }

    pub fn tick(&mut self, s: &Settings, mouse: Vec2, dt: f64) {
        let dt = dt.clamp(1.0 / 1000.0, 1.0 / 25.0);
        let inside = self.screen.contains(mouse);

        if !self.init {
            self.center = mouse;
            self.anchor = mouse;
            self.prev_filtered = mouse;
            self.euro.reset(mouse);
            self.init = true;
        }

        // cursor outside the recorded monitor -> clamp it, keep tracking
        let raw = Vec2::new(
            clampf(
                mouse.x,
                self.screen.x as f32,
                self.screen.right() as f32 - 1.0,
            ),
            clampf(
                mouse.y,
                self.screen.y as f32,
                self.screen.bottom() as f32 - 1.0,
            ),
        );

        let (min_cutoff, beta) = s.euro_params();
        let p = self.euro.filter(raw, dt, min_cutoff, beta);

        let dtf = dt as f32;
        let v = Vec2::new(
            (p.x - self.prev_filtered.x) / dtf,
            (p.y - self.prev_filtered.y) / dtf,
        );
        self.prev_filtered = p;
        let k = (1.0 - (-dt / 0.06).exp()) as f32;
        self.vel_smooth.x += (v.x - self.vel_smooth.x) * k;
        self.vel_smooth.y += (v.y - self.vel_smooth.y) * k;
        let speed = (self.vel_smooth.x * self.vel_smooth.x
            + self.vel_smooth.y * self.vel_smooth.y)
            .sqrt();

        // ---- dynamic viewport size
        let thr = s.speed_threshold.max(1.0);
        let target_expand = smooth_step(speed / thr);
        let tau = if target_expand > self.expand { 0.07 } else { 0.30 };
        let ke = (1.0 - (-dt / tau).exp()) as f32;
        self.expand += (target_expand - self.expand) * ke;

        let out_w = s.out_width as f32;
        let mut src_w = out_w / s.zoom.clamp(0.1, 8.0);
        let aspect = aspect_value(s.aspect_idx);
        let mut src_h = src_w / aspect;

        let grow = 1.0 + (s.speed_expand.clamp(1.0, 4.0) - 1.0) * self.expand;
        src_w *= grow;
        src_h *= grow;

        // never ask for more pixels than the monitor has (keeps the aspect)
        // A viewport bigger than the screen would have to show empty space, so it
        // is shrunk to fit - which silently raises the effective zoom. On a small
        // second monitor that makes every zoom below 1.0 a no-op and kills the
        // speed-driven expansion, so the fact is recorded rather than hidden.
        let mut limited = false;
        if src_w > self.screen.w as f32 {
            let f = self.screen.w as f32 / src_w;
            src_w *= f;
            src_h *= f;
            limited = true;
        }
        if src_h > self.screen.h as f32 {
            let f = self.screen.h as f32 / src_h;
            src_w *= f;
            src_h *= f;
            limited = true;
        }

        // ---- target point: cursor + look-ahead
        let mut target = if inside {
            let look = clampf(s.lookahead_ms, 0.0, 400.0) / 1000.0;
            Vec2::new(
                p.x + clampf(self.vel_smooth.x, -4000.0, 4000.0) * look,
                p.y + clampf(self.vel_smooth.y, -4000.0, 4000.0) * look,
            )
        } else {
            self.center
        };

        // ---- dead zone
        // The camera sits on `anchor`, which follows the pointer slowly. Nothing
        // moves while the pointer is within the zone; beyond it the anchor is
        // dragged just far enough to keep the pointer on the edge. Circling in a
        // small area therefore parks the frame on the centre of that area.
        if s.dead_zone > 0.0005 && inside {
            let rest_w = crate::camera::requested_viewport(s).0.max(16.0);
            let dz = rest_w * s.dead_zone.clamp(0.0, 0.25);
            // ~0.9 s time constant: long enough to average out a circling hand,
            // short enough that walking across the screen does not feel detached
            let k = (dt as f32 / ANCHOR_SECS).clamp(0.0, 1.0);
            self.anchor.x += (p.x - self.anchor.x) * k;
            self.anchor.y += (p.y - self.anchor.y) * k;
            let dx = target.x - self.anchor.x;
            let dy = target.y - self.anchor.y;
            let d = (dx * dx + dy * dy).sqrt();
            if d <= dz {
                target = self.anchor;
            } else {
                let f = (d - dz) / d;
                target = Vec2::new(self.anchor.x + dx * f, self.anchor.y + dy * f);
            }
        }

        if s.clamp_to_screen {
            let hx = src_w * 0.5;
            let hy = src_h * 0.5;
            target.x = if src_w <= self.screen.w as f32 {
                clampf(
                    target.x,
                    self.screen.x as f32 + hx,
                    self.screen.right() as f32 - hx,
                )
            } else {
                self.screen.x as f32 + self.screen.w as f32 * 0.5
            };
            target.y = if src_h <= self.screen.h as f32 {
                clampf(
                    target.y,
                    self.screen.y as f32 + hy,
                    self.screen.bottom() as f32 - hy,
                )
            } else {
                self.screen.y as f32 + self.screen.h as f32 * 0.5
            };
        }

        // ---- critically damped spring
        let w = 2.0 * std::f32::consts::PI * s.follow_hz.clamp(0.2, 20.0);
        let mut n = 1;
        let mut h = dt;
        if dt > 1.0 / 120.0 {
            n = (dt * 120.0).ceil() as i32;
            h = dt / n as f64;
        }
        for _ in 0..n {
            let ax = (target.x - self.center.x) * w * w - self.vel.x * 2.0 * w;
            let ay = (target.y - self.center.y) * w * w - self.vel.y * 2.0 * w;
            self.vel.x += ax * h as f32;
            self.vel.y += ay * h as f32;
            self.center.x += self.vel.x * h as f32;
            self.center.y += self.vel.y * h as f32;
        }

        if s.clamp_to_screen {
            let hx = src_w * 0.5;
            let hy = src_h * 0.5;
            let l = self.screen.x as f32 + hx;
            let r = self.screen.right() as f32 - hx;
            let t = self.screen.y as f32 + hy;
            let b = self.screen.bottom() as f32 - hy;
            if self.center.x < l {
                self.center.x = l;
                if self.vel.x < 0.0 {
                    self.vel.x = 0.0;
                }
            }
            if self.center.x > r {
                self.center.x = r;
                if self.vel.x > 0.0 {
                    self.vel.x = 0.0;
                }
            }
            if self.center.y < t {
                self.center.y = t;
                if self.vel.y < 0.0 {
                    self.vel.y = 0.0;
                }
            }
            if self.center.y > b {
                self.center.y = b;
                if self.vel.y > 0.0 {
                    self.vel.y = 0.0;
                }
            }
        }

        self.snap = CamSnapshot {
            cursor: mouse,
            center: self.center,
            src_w,
            src_h,
            speed,
            expand: self.expand,
            limited,
            valid: true,
            tracking: inside,
        };
    }

    /// Integer source rectangle, clamped to the recorded monitor.
    pub fn source_rect(&self) -> IRect {
        let mut w = self.snap.src_w.round() as i32;
        let mut h = self.snap.src_h.round() as i32;
        w = (w.max(2)) & !1;
        h = (h.max(2)) & !1;
        let mut x = (self.center.x - w as f32 * 0.5).round() as i32;
        let mut y = (self.center.y - h as f32 * 0.5).round() as i32;
        if x < self.screen.x {
            x = self.screen.x;
        }
        if y < self.screen.y {
            y = self.screen.y;
        }
        if x + w > self.screen.right() {
            x = self.screen.right() - w;
        }
        if y + h > self.screen.bottom() {
            y = self.screen.bottom() - h;
        }
        IRect::new(x, y, w, h)
    }
}

#[cfg(test)]
mod cursor_scale {
    use super::growth_factor;


    fn resting_viewport_does_not_scale_the_cursor() {
        // src_w equals out_w/zoom, i.e. no speed expansion
        assert!((growth_factor(1080.0, 1080, 1.0) - 1.0).abs() < 1e-6);
        assert!((growth_factor(540.0, 1080, 2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn expansion_scales_the_cursor_by_the_same_factor() {
        // 1.2x and 1.5x expansion at zoom 1
        assert!((growth_factor(1296.0, 1080, 1.0) - 1.2).abs() < 1e-4);
        assert!((growth_factor(1620.0, 1080, 1.0) - 1.5).abs() < 1e-4);
    }

    #[test]
    fn growth_is_clamped_and_never_shrinks() {
        assert_eq!(growth_factor(900.0, 1080, 1.0), 1.0); // smaller than rest -> 1.0
        assert_eq!(growth_factor(99_999.0, 1080, 1.0), 4.0); // absurd -> 4.0
    }
}

#[cfg(test)]
mod camera_follow {
    use super::*;

    /// A hand circling in one spot must not take the frame with it.
    ///
    /// This drives the same `tick` the recorder calls 250 times a second, so it
    /// covers the live recording path, not only the export.
    fn circling_travel(radius: f32, dead_zone: f32) -> f64 {
        let mut s = Settings::default();
        s.dead_zone = dead_zone;
        s.zoom = 1.0;
        s.out_width = 1080;
        s.speed_resp = 35.0;
        let mut cam = Camera::new();
        cam.set_screen(IRect::new(0, 0, 2560, 1440));
        let c = Vec2::new(1280.0, 720.0);
        for _ in 0..400 {
            cam.tick(&s, c, 1.0 / 250.0);
        }
        let mut total = 0.0f64;
        let mut prev = cam.center;
        for i in 0..(8 * 250) {
            let t = i as f32 / 250.0;
            let a = 2.0 * std::f32::consts::PI * 1.6 * t;
            // a 70 px circle plus the tremor a real hand has
            let p = Vec2::new(
                c.x + radius * a.cos() + (a * 7.0).sin() * 1.2,
                c.y + radius * a.sin() + (a * 9.0).cos() * 1.2,
            );
            cam.tick(&s, p, 1.0 / 250.0);
            total += (((cam.center.x - prev.x).powi(2) + (cam.center.y - prev.y).powi(2))
                .sqrt()) as f64;
            prev = cam.center;
        }
        total
    }

    #[test]
    fn a_circle_inside_the_dead_zone_leaves_the_frame_still() {
        // 50 px radius plus tremor sits well inside the 7 % zone (75 px)
        let on = circling_travel(50.0, 0.07);
        assert!(on < 40.0, "camera should hold still, it travelled {on:.0} px");
    }

    #[test]
    fn a_circle_larger_than_the_zone_is_damped_a_lot() {
        // 70 px is right at the edge of the zone, so it rubs against it
        let off = circling_travel(70.0, 0.0);
        let on = circling_travel(70.0, 0.07);
        assert!(
            on < off * 0.4,
            "dead zone should damp it: off={off:.0} px on={on:.0} px"
        );
    }

    #[test]
    fn dead_zone_can_be_switched_off() {
        // dz 0 has to behave like it did before the feature existed
        let off = circling_travel(70.0, 0.0);
        assert!(off > 500.0, "without a dead zone the frame follows: {off:.0} px");
    }
}
