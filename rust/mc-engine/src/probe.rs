//! Post-recording health report.
//!
//! Everything here is derived from what a take already wrote (`meta.json`,
//! `frames.csv`, `track.csv`, the media files), so it can be run on old material
//! and never needs the recorder to still be alive.
//!
//! The point is that a bad take should say *what* is wrong instead of just
//! looking odd: a frozen opening, a drifting audio track, or a camera pinned to
//! a screen edge all have a signature in those files.

use std::path::Path;

use crate::ffmpeg::media_duration;

/// One line of `frames.csv`. Duplicates carry only `frame,tick`.
struct Entry {
    tick: i64,
    rect: Option<(f32, f32, f32, f32)>,
}

pub struct Probe {
    pub frames: i64,
    pub duplicates: i64,
    /// Longest run of frames whose camera tick never advanced, near the start.
    pub leading_frozen_ms: f64,
    /// Frames anywhere whose tick came out the same as the previous frame.
    pub stalled_frames: i64,
    /// Longest stalled run that sits at the head of the take.
    pub leading_frozen_frames: i64,
    pub tick_regressions: i64,
    pub duration: f64,
    pub video_secs: f64,
    pub audio_secs: f64,
    pub dropped: u32,
    pub clamped_frames: i64,
    pub issues: Vec<String>,
}

impl Probe {
    pub fn good(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Reads a take and reports anything that would make it look wrong.
pub fn probe(dir: &Path, ffmpeg: Option<&Path>) -> Result<Probe, String> {
    let meta_path = dir.join("meta.json");
    let meta = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("读取 {} 失败: {e}", meta_path.display()))?;

    let fps = json_num(&meta, "fps", 30.0).max(1.0);
    let duration = json_num(&meta, "duration", 0.0);
    let dropped = json_num(&meta, "dropped", 0.0) as u32;
    let (mon_x, mon_y, mon_w, mon_h) = parse_monitor(&meta).unwrap_or((0.0, 0.0, 0.0, 0.0));

    let entries = read_frames(&dir.join("frames.csv"))?;
    let frames = entries.len() as i64;
    let duplicates = entries.iter().filter(|e| e.rect.is_none()).count() as i64;

    // A frozen opening is a run of frames whose tick never advances: the encoder
    // was still warming up and the recorder duplicated frames to hold the clock,
    // so the camera - and therefore the preview - sits still. Two frames of
    // pre-roll are by design; a longer run near the start is a stall.
    let mut best_run = 1i64;
    let mut best_start = 0i64;
    let mut run = 1i64;
    for i in 1..entries.len() {
        if entries[i].tick == entries[i - 1].tick {
            run += 1;
            if run > best_run {
                best_run = run;
                best_start = i as i64 + 1 - run;
            }
        } else {
            run = 1;
        }
    }
    let in_the_opening = (best_start as f64 / fps) < 1.5;
    let leading_frozen = if best_run > 2 && in_the_opening {
        best_run - 2
    } else {
        0
    };

    // Repeats are stalls (the camera stood still), decreases are real
    // regressions - the clock went backwards - and only the latter is a bug.
    let mut stalled = 0i64;
    let mut regressions = 0i64;
    for w in entries.windows(2).skip(2) {
        if w[1].tick == w[0].tick {
            stalled += 1;
        } else if w[1].tick < w[0].tick {
            regressions += 1;
        }
    }

    // How often the viewport is pinned against a monitor edge, which is what a
    // cursor resting in a corner looks like.
    let mut clamped = 0i64;
    if mon_w > 0.0 && mon_h > 0.0 {
        for e in &entries {
            let Some((cx, cy, w, h)) = e.rect else {
                continue;
            };
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            let (hx, hy) = (w * 0.5, h * 0.5);
            let at_left = (cx - hx - mon_x).abs() < 1.5;
            let at_right = (cx + hx - (mon_x + mon_w)).abs() < 1.5;
            let at_top = (cy - hy - mon_y).abs() < 1.5;
            let at_bottom = (cy + hy - (mon_y + mon_h)).abs() < 1.5;
            if at_left || at_right || at_top || at_bottom {
                clamped += 1;
            }
        }
    }

    let video_secs = if duration > 0.0 {
        duration
    } else if let Some(ff) = ffmpeg {
        media_duration(ff, &dir.join("master.mp4"))
    } else {
        0.0
    };
    let audio_secs = wav_seconds(dir);

    let mut issues = Vec::new();
    if leading_frozen > 0 {
        issues.push(format!(
            "开头约 {:.0} ms 画面是静止的（{} 帧没有推进取景）",
            leading_frozen as f64 * 1000.0 / fps,
            leading_frozen
        ));
    }
    if stalled > leading_frozen {
        issues.push(format!(
            "另有 {} 帧取景没有推进",
            stalled - leading_frozen
        ));
    }
    if regressions > 0 {
        issues.push(format!("有 {regressions} 处取景刻度倒退了"));
    }
    if video_secs > 0.0 && audio_secs > 0.0 && (video_secs - audio_secs).abs() > 0.25 {
        issues.push(format!(
            "音画时长差 {:.2} 秒（视频 {:.2}s / 音频 {:.2}s）",
            video_secs - audio_secs,
            video_secs,
            audio_secs
        ));
    }
    if dropped > 0 {
        issues.push(format!("采集端丢帧 {dropped} 帧"));
    }
    // Clamping is a consequence of where the cursor sat, not a fault, so it stays
    // a number in the report instead of an issue.

    Ok(Probe {
        frames,
        duplicates,
        leading_frozen_ms: leading_frozen as f64 * 1000.0 / fps,
        stalled_frames: stalled,
        leading_frozen_frames: leading_frozen,
        tick_regressions: regressions,
        duration,
        video_secs,
        audio_secs,
        dropped,
        clamped_frames: clamped,
        issues,
    })
}

/// `frame,tick,cx,cy,w,h` for a real frame, `frame,tick` for a duplicate.
fn read_frames(path: &Path) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 2 {
            continue;
        }
        let tick = parts[1].trim().parse::<i64>().unwrap_or(0);
        let rect = if parts.len() >= 6 {
            Some((
                parts[2].trim().parse().unwrap_or(0.0),
                parts[3].trim().parse().unwrap_or(0.0),
                parts[4].trim().parse().unwrap_or(0.0),
                parts[5].trim().parse().unwrap_or(0.0),
            ))
        } else {
            None
        };
        out.push(Entry { tick, rect });
    }
    Ok(out)
}

/// The two captured sources run in parallel and are mixed, so the track length
/// is the longest of them, not their sum.
fn wav_seconds(dir: &Path) -> f64 {
    let mut longest = 0.0f64;
    for name in ["audio_system.wav", "audio_mic.wav"] {
        if let Ok(md) = std::fs::metadata(dir.join(name)) {
            if md.len() > 44 {
                longest = longest.max((md.len() - 44) as f64 / 192_000.0);
            }
        }
    }
    longest
}

fn json_num(text: &str, key: &str, fallback: f64) -> f64 {
    let pat = format!("\"{key}\":");
    let Some(i) = text.find(&pat) else {
        return fallback;
    };
    let rest = text[i + pat.len()..].trim_start();
    let end = rest
        .find(|c: char| c != '-' && c != '.' && !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].trim().parse().unwrap_or(fallback)
}

fn parse_monitor(text: &str) -> Option<(f32, f32, f32, f32)> {
    let i = text.find("\"monitor\"")?;
    let rest = &text[i..];
    Some((
        json_num(rest, "x", 0.0) as f32,
        json_num(rest, "y", 0.0) as f32,
        json_num(rest, "w", 0.0) as f32,
        json_num(rest, "h", 0.0) as f32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn take(name: &str, frames_csv: &str, meta: &str) -> std::path::PathBuf {
        // one directory per test: cargo runs these in parallel
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("probe-test")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("frames.csv"))
            .unwrap()
            .write_all(frames_csv.as_bytes())
            .unwrap();
        std::fs::File::create(dir.join("meta.json"))
            .unwrap()
            .write_all(meta.as_bytes())
            .unwrap();
        dir
    }

    const META: &str =
        r#"{"fps":30,"duration":4.0,"frames":120,"monitor":{"x":0,"y":0,"w":2560,"h":1440}}"#;

    #[test]
    fn healthy_take_has_no_issues() {
        let mut csv = String::from("# frame,camera_tick,cx,cy,w,h\n");
        for i in 0..120 {
            let tick = if i < 2 { 1 } else { 1 + (i - 1) * 8 };
            csv.push_str(&format!("{i},{tick},1000.0,700.0,1080.0,608.0\n"));
        }
        let dir = take("healthy", &csv, META);
        let p = probe(&dir, None).unwrap();
        assert_eq!(p.duplicates, 0);
        assert_eq!(p.leading_frozen_ms, 0.0);
        assert_eq!(p.tick_regressions, 0);
        assert!(p.good(), "unexpected issues: {:?}", p.issues);
    }

    #[test]
    fn frozen_opening_is_reported() {
        let mut csv = String::from("# frame,camera_tick,cx,cy,w,h\n");
        // the shape a take had before the clock moved past the encoder warm-up:
        // frame 1 onwards repeats the same tick for a quarter of a second
        for i in 0..24 {
            let (tick, full) = match i {
                0 => (1, true),
                1..=8 => (17, i == 1),
                _ => (17 + (i as i64 - 8) * 8, true),
            };
            if full {
                csv.push_str(&format!("{i},{tick},800.0,400.0,450.0,253.0\n"));
            } else {
                csv.push_str(&format!("{i},{tick}\n"));
            }
        }
        let dir = take("frozen", &csv, META);
        let p = probe(&dir, None).unwrap();
        assert_eq!(p.duplicates, 7);
        assert!(p.leading_frozen_ms > 150.0, "got {}", p.leading_frozen_ms);
        assert!(p.issues.iter().any(|s| s.contains("静止")), "{:?}", p.issues);
    }

    #[test]
    fn tick_that_goes_backwards_is_reported() {
        let mut csv = String::from("# frame,camera_tick,cx,cy,w,h\n");
        for (i, tick) in [1, 1, 9, 17, 17, 25].iter().enumerate() {
            csv.push_str(&format!("{i},{tick},1000.0,700.0,1080.0,608.0\n"));
        }
        let dir = take("backwards", &csv, META);
        let p = probe(&dir, None).unwrap();
        assert!(p.stalled_frames >= 1);
        assert!(p.issues.iter().any(|s| s.contains("倒退") || s.contains("推进")));
    }
}
