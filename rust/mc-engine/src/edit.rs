//! Edit list (`.mcl`): trims, manual follow-point keyframes and camera
//! parameters. Line based on purpose so the UI and a human edit the same text.

use crate::settings::Settings;
use crate::track::TrackPt;

/// `< 0` means "keep whatever meta.json says".
#[derive(Clone, Copy, Debug)]
pub struct RenderOpts {
    pub zoom: f32,
    pub expand: f32,
    pub threshold: f32,
    pub smooth: f32,
    pub resp: f32,
    pub follow: f32,
    pub look: f32,
    pub aspect: i32,
    pub width: i32,
    pub fps: i32,
    /// dead zone in percent of the viewport width; -1 keeps meta.json's value
    pub dz: f32,
    /// pointer smoothing in ms; -1 keeps meta.json's value
    pub cs: f32,
    /// click ring: -1 keeps meta.json's value, 0 off, 1 on
    pub clk: i32,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            zoom: -1.0,
            expand: -1.0,
            threshold: -1.0,
            smooth: -1.0,
            resp: -1.0,
            follow: -1.0,
            look: -1.0,
            aspect: -1,
            width: -1,
            fps: -1,
            dz: -1.0,
            cs: -1.0,
            clk: -1,
        }
    }
}

impl RenderOpts {
    pub fn apply(&self, s: &mut Settings) {
        if self.zoom > 0.0 {
            s.zoom = self.zoom;
        }
        if self.expand > 0.0 {
            s.speed_expand = self.expand;
        }
        if self.threshold > 0.0 {
            s.speed_threshold = self.threshold;
        }
        if self.smooth >= 0.0 {
            s.smooth = self.smooth;
        }
        if self.resp >= 0.0 {
            s.speed_resp = self.resp;
        }
        if self.follow > 0.0 {
            s.follow_hz = self.follow;
        }
        if self.look >= 0.0 {
            s.lookahead_ms = self.look;
        }
        if self.aspect >= 0 && (self.aspect as usize) < crate::settings::ASPECTS.len() {
            s.aspect_idx = self.aspect as usize;
        }
        if self.width > 0 {
            s.out_width = self.width;
        }
        if self.fps > 0 {
            s.fps = self.fps;
        }
        if self.dz >= 0.0 {
            s.dead_zone = (self.dz / 100.0).clamp(0.0, 0.25);
        }
        if self.cs >= 0.0 {
            s.cursor_smooth_ms = self.cs.clamp(0.0, 250.0);
        }
        if self.clk >= 0 {
            s.click_ring = self.clk != 0;
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EditList {
    pub trims: Vec<(f64, f64)>,
    pub kf: Vec<TrackPt>,
    pub cam: RenderOpts,
}

pub fn parse_edit(text: &str) -> EditList {
    let mut e = EditList::default();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, |c: char| c.is_whitespace());
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest = parts.next().unwrap_or("").trim();
        if rest.starts_with('#') {
            continue;
        }

        match key.as_str() {
            "trim" => {
                let v: Vec<f64> = rest
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if v.len() >= 2 && v[1] > v[0] {
                    e.trims.push((v[0], v[1]));
                }
            }
            "kf" => {
                let v: Vec<f64> = rest
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if v.len() >= 3 {
                    e.kf.push(TrackPt {
                        t: v[0].max(0.0),
                        x: v[1] as i32,
                        y: v[2] as i32,
                        btn: 0,
                    });
                }
            }
            "zoom" => e.cam.zoom = rest.parse().unwrap_or(e.cam.zoom),
            "expand" => e.cam.expand = rest.parse().unwrap_or(e.cam.expand),
            "threshold" => e.cam.threshold = rest.parse().unwrap_or(e.cam.threshold),
            "smooth" => e.cam.smooth = rest.parse().unwrap_or(e.cam.smooth),
            "resp" => e.cam.resp = rest.parse().unwrap_or(e.cam.resp),
            "follow" => e.cam.follow = rest.parse().unwrap_or(e.cam.follow),
            "look" => e.cam.look = rest.parse().unwrap_or(e.cam.look),
            "aspect" => e.cam.aspect = rest.parse().unwrap_or(e.cam.aspect),
            "width" => e.cam.width = rest.parse().unwrap_or(e.cam.width),
            "fps" => e.cam.fps = rest.parse().unwrap_or(e.cam.fps),
            "dz" => e.cam.dz = rest.parse().unwrap_or(e.cam.dz),
            "cs" => e.cam.cs = rest.parse().unwrap_or(e.cam.cs),
            "clk" => e.cam.clk = rest.parse().unwrap_or(e.cam.clk),
            _ => {}
        }
    }

    if e.kf.len() < 2 {
        e.kf.clear();
    }
    e.kf.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    e
}

/// Where the camera should aim: manual keyframes win inside their span.
/// Every left-button press in the track, as (time, x, y).
///
/// `btn` bit 0 is the left button, so a press is the sample where it goes from
/// clear to set. Holding the button down does not produce more than one.
pub fn click_points(track: &[TrackPt]) -> Vec<(f64, f32, f32)> {
    let mut out = Vec::new();
    let mut down = false;
    for p in track {
        let now = (p.btn & 1) != 0;
        if now && !down {
            out.push((p.t, p.x as f32, p.y as f32));
        }
        down = now;
    }
    out
}

pub fn cursor_at(track: &[TrackPt], edit: &EditList, t: f64) -> crate::camera::Vec2 {
    if edit.kf.len() >= 2 {
        let k = &edit.kf;
        if t >= k[0].t && t <= k[k.len() - 1].t {
            for i in 0..k.len() - 1 {
                if t >= k[i].t && t <= k[i + 1].t {
                    let span = k[i + 1].t - k[i].t;
                    let f = if span > 1e-9 {
                        (t - k[i].t) / span
                    } else {
                        0.0
                    };
                    return crate::camera::Vec2::new(
                        (k[i].x as f64 + (k[i + 1].x - k[i].x) as f64 * f) as f32,
                        (k[i].y as f64 + (k[i + 1].y - k[i].y) as f64 * f) as f32,
                    );
                }
            }
        }
    }
    crate::track::at(track, t)
}

/// output frame index -> source frame index, honouring the trims
/// Resamples a source-frame map onto a different output rate.
///
/// `build_frame_map` answers "which source frames were kept"; the writer stamps
/// its output at the rate it was given, so writing one frame per kept source
/// frame at another rate changes the playback speed instead of the frame rate.
/// This picks, for every output frame, the kept frame that was on screen then.
pub fn resample_map(fmap: &[i32], clip_fps: i32, out_fps: i32) -> Vec<i32> {
    if fmap.is_empty() || clip_fps <= 0 || out_fps <= 0 || clip_fps == out_fps {
        return fmap.to_vec();
    }
    let n = ((fmap.len() as f64) * (out_fps as f64) / (clip_fps as f64))
        .round()
        .max(1.0) as usize;
    (0..n)
        .map(|k| {
            let i = ((k as f64) * (clip_fps as f64) / (out_fps as f64)).floor() as usize;
            fmap[i.min(fmap.len() - 1)]
        })
        .collect()
}

pub fn build_frame_map(frame_count: i32, fps: i32, edit: &EditList) -> Vec<i32> {
    if edit.trims.is_empty() {
        return (0..frame_count).collect();
    }
    let mut map: Vec<i32> = Vec::new();
    for (a, b) in &edit.trims {
        let start = ((a * fps as f64).floor() as i32).max(0);
        let end = ((b * fps as f64).ceil() as i32).min(frame_count);
        for i in start..end {
            if map.last().map_or(true, |&last| i > last) {
                map.push(i);
            }
        }
    }
    map
}

#[cfg(test)]
mod resample_tests {
    use super::*;

    /// The frame count has to follow the requested rate while the *duration*
    /// stays put. Getting this wrong is what turned a 30 fps take into a
    /// double-speed export.
    #[test]
    fn rate_change_keeps_the_duration() {
        let src: Vec<i32> = (0..108).collect();
        for out in [24, 30, 50, 60, 120] {
            let got = resample_map(&src, 30, out);
            let want_s = 108.0 / 30.0;
            let got_s = got.len() as f64 / out as f64;
            assert!(
                (got_s - want_s).abs() <= 1.0 / out as f64 + 1e-9,
                "30->{out}: {got_len} frames = {got_s}s, want {want_s}s",
                got_len = got.len()
            );
        }
    }

    #[test]
    fn same_rate_is_the_identity() {
        let src = vec![0, 1, 2, 3, 4];
        assert_eq!(resample_map(&src, 30, 30), src);
        assert_eq!(resample_map(&src, 60, 60), src);
    }

    #[test]
    fn output_stays_inside_the_source_range() {
        let src: Vec<i32> = (10..40).collect();
        for out in [12, 24, 60, 240] {
            let got = resample_map(&src, 30, out);
            assert!(!got.is_empty());
            assert!(got[0] >= 10 && *got.last().unwrap() < 40);
            assert!(got.windows(2).all(|w| w[0] <= w[1]), "must not go backwards");
        }
    }

    #[test]
    fn empty_and_bad_rates_do_not_panic() {
        assert!(resample_map(&[], 30, 60).is_empty());
        assert_eq!(resample_map(&[1, 2], 0, 60), vec![1, 2]);
        assert_eq!(resample_map(&[1, 2], 30, 0), vec![1, 2]);
    }
}
