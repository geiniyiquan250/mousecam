//! Audio capture: the desktop's own sound (WASAPI loopback) and the microphone,
//! each written to its own WAV.
//!
//! They stay in separate files rather than being mixed live so that a source can
//! fail without taking the other one down, and so the mix can be redone at mux
//! time. Recording never depends on audio: any failure is reported and the video
//! continues.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

const RATE: usize = 48_000;
const CHANNELS: usize = 2;
const BLOCK: usize = CHANNELS * 2; // 16-bit stereo
const BYTES_PER_SEC: usize = RATE * BLOCK;

/// What one capture stream produced. A source can fail without stopping the
/// recording, so the failure travels back with the file rather than being lost.
pub struct AudioFile {
    pub path: PathBuf,
    pub error: Option<String>,
}

impl AudioFile {
    pub fn usable(&self) -> bool {
        self.error.is_none() && self.path.exists() && self.path.metadata().map(|m| m.len() > 44).unwrap_or(false)
    }
}

/// One capture stream, stopped and finalised by [`AudioSession::stop`].
pub struct AudioSession {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    files: Vec<(PathBuf, Arc<Mutex<Option<String>>>)>,
}

impl AudioSession {
    /// Stops every stream and reports what each one produced.
    pub fn stop(self) -> Vec<AudioFile> {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
        self.files
            .into_iter()
            .map(|(path, err)| {
                let error = err.lock().unwrap().take();
                if let Some(e) = &error {
                    crate::mc_warn!("audio", "{} 采集失败: {e}", path.display());
                }
                AudioFile { path, error }
            })
            .collect()
    }
}

/// Starts the requested sources. Returns `None` when neither was asked for.
pub fn start(dir: &Path, system: bool, mic: bool) -> Option<AudioSession> {
    if !system && !mic {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let mut files = Vec::new();

    for (enabled, is_system, name) in [(system, true, "audio_system.wav"), (mic, false, "audio_mic.wav")] {
        if !enabled {
            continue;
        }
        let path = dir.join(name);
        let err = Arc::new(Mutex::new(None));
        let stop_flag = stop.clone();
        let target = path.clone();
        let slot = err.clone();
        let handle = std::thread::Builder::new()
            .name(format!("audio-{}", if is_system { "sys" } else { "mic" }))
            .spawn(move || {
                crate::mc_info!(
                    "audio",
                    "开始采集{}",
                    if is_system { "系统声音" } else { "麦克风" }
                );
                match run_capture(is_system, &target, &stop_flag) {
                    Ok(()) => crate::mc_info!("audio", "{} 采集结束", target.display()),
                    Err(e) => {
                        crate::mc_warn!("audio", "{} 采集失败: {e}", target.display());
                        *slot.lock().unwrap() = Some(e);
                    }
                }
            })
            .ok()?;
        threads.push(handle);
        files.push((path, err));
    }

    Some(AudioSession {
        stop,
        threads,
        files,
    })
}

/// True when a default device exists for each direction, so the UI can grey out
/// a switch that cannot work.
pub fn available() -> (bool, bool) {
    let _ = initialize_mta();
    let Ok(en) = DeviceEnumerator::new() else {
        return (false, false);
    };
    let sys = en.get_default_device(&Direction::Render).is_ok();
    let mic = en.get_default_device(&Direction::Capture).is_ok();
    (sys, mic)
}

fn run_capture(is_system: bool, path: &Path, stop: &AtomicBool) -> Result<(), String> {
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new().map_err(|e| format!("枚举音频设备失败: {e}"))?;
    // Loopback is a capture stream activated on the *render* device: the device
    // direction picks the endpoint, the requested direction must still be
    // `Capture`, which is what makes the crate set AUDCLNT_STREAMFLAGS_LOOPBACK.
    let endpoint = if is_system {
        Direction::Render
    } else {
        Direction::Capture
    };
    let stream = Direction::Capture;
    let device = enumerator
        .get_default_device(&endpoint)
        .map_err(|e| format!("找不到默认音频设备: {e}"))?;
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| format!("打开音频设备失败: {e}"))?;

    let format = WaveFormat::new(16, 16, &SampleType::Int, RATE, CHANNELS, None);
    let (_, min_period) = client
        .get_device_period()
        .map_err(|e| format!("读取设备周期失败: {e}"))?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_period,
    };
    client
        .initialize_client(&format, &stream, &mode)
        .map_err(|e| format!("初始化音频流失败: {e}"))?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("音频事件句柄失败: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("获取采集接口失败: {e}"))?;
    let mut wav = WavWriter::create(path).map_err(|e| format!("创建音频文件失败: {e}"))?;

    client
        .start_stream()
        .map_err(|e| format!("启动音频流失败: {e}"))?;
    let started = Instant::now();
    let mut queue: VecDeque<u8> = VecDeque::with_capacity(BYTES_PER_SEC);

    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(120).is_ok() {
            let _ = capture.read_from_device_to_deque(&mut queue);
        }
        let whole = queue.len() - queue.len() % BLOCK;
        if whole > 0 {
            let chunk: Vec<u8> = queue.drain(..whole).collect();
            if wav.write(&chunk).is_err() {
                break;
            }
        }
        // Loopback delivers nothing while the machine is silent. Without padding
        // the track would drift behind the video by however long the silence
        // lasted, and a quiet moment would desynchronise the whole recording.
        let expected = (started.elapsed().as_secs_f64() * BYTES_PER_SEC as f64) as u64;
        if expected > wav.written + (BYTES_PER_SEC as u64 / 20) {
            let mut gap = (expected - wav.written) as usize;
            gap -= gap % BLOCK;
            gap = gap.min(BYTES_PER_SEC * 2);
            if gap > 0 && wav.write(&vec![0u8; gap]).is_err() {
                break;
            }
        }
    }

    let _ = client.stop_stream();
    wav.finish();
    Ok(())
}

struct WavWriter {
    out: BufWriter<File>,
    written: u64,
}

impl WavWriter {
    fn create(path: &Path) -> std::io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(&header(0))?;
        Ok(Self { out, written: 0 })
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn finish(mut self) {
        let _ = self.out.flush();
        let file = self.out.get_mut();
        if file.seek(SeekFrom::Start(0)).is_ok() {
            let _ = file.write_all(&header(self.written));
            let _ = file.flush();
        }
    }
}

fn header(data_bytes: u64) -> [u8; 44] {
    let mut h = [0u8; 44];
    let riff = 36 + data_bytes as u32;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&riff.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&(CHANNELS as u16).to_le_bytes());
    h[24..28].copy_from_slice(&(RATE as u32).to_le_bytes());
    h[28..32].copy_from_slice(&((RATE * BLOCK) as u32).to_le_bytes());
    h[32..34].copy_from_slice(&(BLOCK as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&(data_bytes as u32).to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_16bit_stereo_48k() {
        let h = header(0);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 2); // channels
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 48_000);
        assert_eq!(u16::from_le_bytes([h[32], h[33]]), 4); // block align
        assert_eq!(u16::from_le_bytes([h[34], h[35]]), 16); // bits
    }

    #[test]
    fn wav_header_records_the_payload_size() {
        let h = header(1000);
        assert_eq!(u32::from_le_bytes([h[4], h[5], h[6], h[7]]), 1036);
        assert_eq!(u32::from_le_bytes([h[40], h[41], h[42], h[43]]), 1000);
    }

    /// Live check: needs a default playback device. Proves the whole chain
    /// (device -> client -> event -> WAV) rather than just the header maths.
    #[test]
    fn captures_system_audio_for_a_moment() {
        // %TEMP% is not writable on every machine, so stay inside the crate.
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("audio-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = start(&dir, true, false).expect("no session");
        std::thread::sleep(std::time::Duration::from_millis(900));
        let files = session.stop();
        assert_eq!(files.len(), 1, "expected exactly one stream");
        if let Some(e) = &files[0].error {
            panic!("system capture failed: {e}");
        }
        assert!(files[0].usable(), "captured file is empty");
        let len = std::fs::metadata(&files[0].path).unwrap().len();
        assert!(
            len > 44 + 48_000,
            "expected around one second of 48kHz stereo, got {len} bytes"
        );
    }

    /// The microphone is optional: a machine without one must degrade quietly
    /// instead of breaking the recording.
    #[test]
    fn mic_capture_is_optional() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("audio-test-mic");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = start(&dir, false, true).expect("no session");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let files = session.stop();
        assert_eq!(files.len(), 1);
        match &files[0].error {
            Some(e) => eprintln!("no microphone on this machine: {e}"),
            None => assert!(files[0].usable(), "microphone produced an empty file"),
        }
    }
}
