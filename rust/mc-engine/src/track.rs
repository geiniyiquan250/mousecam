//! Recorded mouse trace: `track.csv` written by the recorder.

use crate::camera::Vec2;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct TrackPt {
    pub t: f64,
    pub x: i32,
    pub y: i32,
    pub btn: u32,
}

/// A zero-phase smoothed copy of `track`.
///
/// A hand-held pointer carries 8-12 Hz tremor of a pixel or two. A causal filter
/// would drag the pointer behind the clicks, so this runs forward and then
/// backward over the forward result: the phase cancels, and the pointer stays
/// where it was recorded, just steadier.
pub fn smooth_track(track: &[TrackPt], tau_ms: f32) -> Vec<TrackPt> {
    let n = track.len();
    if tau_ms <= 1.0 || n < 3 {
        return track.to_vec();
    }
    let tau = (tau_ms as f64 / 1000.0).max(1e-4);
    let mut xs = vec![0f64; n];
    let mut ys = vec![0f64; n];

    let (mut px, mut py) = (track[0].x as f64, track[0].y as f64);
    for i in 0..n {
        let dt = if i == 0 {
            0.0
        } else {
            (track[i].t - track[i - 1].t).clamp(0.0, 0.1)
        };
        let k = 1.0 - (-dt / tau).exp();
        px += (track[i].x as f64 - px) * k;
        py += (track[i].y as f64 - py) * k;
        xs[i] = px;
        ys[i] = py;
    }

    let (mut bx, mut by) = (xs[n - 1], ys[n - 1]);
    for i in (0..n).rev() {
        let dt = if i == n - 1 {
            0.0
        } else {
            (track[i + 1].t - track[i].t).clamp(0.0, 0.1)
        };
        let k = 1.0 - (-dt / tau).exp();
        bx += (xs[i] - bx) * k;
        by += (ys[i] - by) * k;
        xs[i] = bx;
        ys[i] = by;
    }

    track
        .iter()
        .enumerate()
        .map(|(i, p)| TrackPt {
            t: p.t,
            x: xs[i].round() as i32,
            y: ys[i].round() as i32,
            btn: p.btn,
        })
        .collect()
}

pub fn load_track(path: &Path) -> std::io::Result<Vec<TrackPt>> {
    let f = File::open(path)?;
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut it = l.split(',');
        let t = match it.next().and_then(|v| v.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let x = it.next().and_then(|v| v.parse::<i32>().ok());
        let y = it.next().and_then(|v| v.parse::<i32>().ok());
        let b = it.next().and_then(|v| v.parse::<u32>().ok());
        if let (Some(x), Some(y)) = (x, y) {
            out.push(TrackPt { t, x, y, btn: b.unwrap_or(0) });
        }
    }
    Ok(out)
}

/// Linear interpolation between samples.
pub fn at(track: &[TrackPt], t: f64) -> Vec2 {
    if track.is_empty() {
        return Vec2::default();
    }
    let first = &track[0];
    let last = &track[track.len() - 1];
    if t <= first.t {
        return Vec2::new(first.x as f32, first.y as f32);
    }
    if t >= last.t {
        return Vec2::new(last.x as f32, last.y as f32);
    }

    let (mut lo, mut hi) = (0usize, track.len() - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if track[mid].t <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let span = track[hi].t - track[lo].t;
    let f = if span > 1e-9 {
        (t - track[lo].t) / span
    } else {
        0.0
    };
    Vec2::new(
        (track[lo].x as f64 + (track[hi].x - track[lo].x) as f64 * f) as f32,
        (track[lo].y as f64 + (track[hi].y - track[lo].y) as f64 * f) as f32,
    )
}

#[cfg(test)]
mod smooth_tests {
    use super::*;

    fn track_of(points: &[(f64, i32, i32)]) -> Vec<TrackPt> {
        points
            .iter()
            .map(|&(t, x, y)| TrackPt { t, x, y, btn: 0 })
            .collect()
    }

    /// A steady hand still shakes; the smoothed pointer should sit near the point
    /// it was held at instead of tracing the tremor.
    #[test]
    fn smoothing_calms_tremor_around_a_held_point() {
        let mut pts = Vec::new();
        for i in 0..1000 {
            let t = i as f64 / 250.0;
            let dx = (t * 2.0 * std::f64::consts::PI * 9.0).sin() * 3.0;
            let dy = (t * 2.0 * std::f64::consts::PI * 11.0).cos() * 3.0;
            pts.push((t, 500 + dx.round() as i32, 400 + dy.round() as i32));
        }
        let raw = track_of(&pts);
        let smooth = smooth_track(&raw, 60.0);
        // the first and last few samples are one-sided, so measure the settled part
        let mid = |v: &[TrackPt]| {
            v[250..750]
                .iter()
                .map(|p| (p.x - 500).abs().max((p.y - 400).abs()))
                .max()
                .unwrap_or(0)
        };
        assert!(mid(&raw) >= 2, "test track needs tremor");
        assert!(
            mid(&smooth) <= 1,
            "tremor should be gone: raw {} px, smoothed {} px",
            mid(&raw),
            mid(&smooth)
        );
    }

    /// Zero phase: a constant-speed move must not be dragged behind.
    #[test]
    fn smoothing_does_not_lag_a_steady_move() {
        let mut pts = Vec::new();
        for i in 0..500 {
            let t = i as f64 / 250.0;
            pts.push((t, 100 + (t * 800.0) as i32, 300));
        }
        let raw = track_of(&pts);
        let smooth = smooth_track(&raw, 60.0);
        // edges are one-sided and can drift a couple of pixels during the first
        // few hundred ms; the settled part must not lag at all
        let mut worst = 0;
        for i in 200..300 {
            worst = worst.max((raw[i].x - smooth[i].x).abs());
        }
        assert!(worst <= 1, "smoothed track lags by {worst} px");
    }

    #[test]
    fn zero_tau_leaves_the_track_alone() {
        let raw = track_of(&[(0.0, 10, 20), (0.01, 11, 21), (0.02, 30, 40)]);
        let out = smooth_track(&raw, 0.0);
        assert_eq!(out.len(), raw.len());
        for (a, b) in raw.iter().zip(out.iter()) {
            assert_eq!((a.x, a.y), (b.x, b.y));
        }
    }
}
