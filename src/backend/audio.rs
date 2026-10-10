//! Voice audio (docs/VOICE.md): the platform-neutral half — the rings
//! between the mixer and the sound card, the microphone chain (RNNoise +
//! voice-activity gate) and the `MediaSource` songbird reads. The device
//! I/O lives per platform with one API (`start`, `AudioLinks`,
//! `list_devices`): `linux.rs` through the desktop sound server
//! (PulseAudio API), `windows.rs` through WASAPI (cpal).

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nnnoiseless::DenoiseState;
use symphonia_core::io::MediaSource;

mod dynamics;
mod effects;
pub use effects::{EFFECTS, Sound, decode, play_sound};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{AudioLinks, list_devices, start};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{AudioLinks, echo_cancel_status, list_devices, start};

/// Discord voice runs at 48 kHz.
pub const VOICE_RATE: u32 = 48_000;

/// Playback ring depth, stereo samples: ≈200 ms to absorb mixer/scheduler
/// jitter without a noticeable delay.
const OUTPUT_CAP: usize = 2 * VOICE_RATE as usize / 5;
/// Microphone ring depth, mono samples: headroom for the burst pattern
/// between the capture callback and songbird's 20 ms mixer pulls.
const INPUT_CAP: usize = VOICE_RATE as usize / 10;
/// Stereo samples (3 ticks ≈ 60 ms) to accumulate before starting playback;
/// without it, the mixer's 20 ms cadence sounds like constant crackle.
const PREBUFFER: usize = 3 * 2 * VOICE_RATE as usize / 50;
/// How long the voice-activity gate stays open after audio drops below the
/// sensitivity threshold, so word tails aren't clipped.
const VAD_HANGOVER: Duration = Duration::from_millis(300);
/// Minimum RMS that lights the speaking indicator, so it isn't stuck on
/// when the sensitivity is 0 (gate always open).
const SPEAKING_FLOOR: f32 = 0.01;

/// f32 sample ring shared by a producer (mixer or capture callback) and a
/// consumer (pulse callback or songbird's mixer). Underruns read as silence;
/// overflows drop the oldest audio.
pub struct Ring {
    samples: Mutex<VecDeque<f32>>,
    cap: usize,
    volume: AtomicU8,
}

impl Ring {
    fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            samples: Mutex::new(VecDeque::with_capacity(cap.min(4096))),
            cap,
            volume: AtomicU8::new(100),
        })
    }

    /// A standalone ring attached to no device: the silent microphone
    /// placeholder the driver holds between calls.
    pub fn detached() -> Arc<Self> {
        Self::new(INPUT_CAP)
    }

    /// Appends fresh audio (a finished mix tick or mic capture), dropping the
    /// oldest on overflow.
    pub fn push(&self, add: &[f32]) {
        let mut buf = self.samples.lock().unwrap();
        buf.extend(add.iter().copied());
        while buf.len() > self.cap {
            buf.pop_front();
        }
    }

    /// Pops exactly `out.len()` samples; underruns become silence.
    pub fn pop(&self, out: &mut [f32]) {
        let mut buf = self.samples.lock().unwrap();
        for slot in out.iter_mut() {
            *slot = buf.pop_front().unwrap_or(0.0);
        }
    }

    pub fn set_volume(&self, percent: u8) {
        self.volume.store(percent.min(200), Ordering::Relaxed);
    }

    /// Gain is applied after effects so every local voice sound follows it.
    fn apply_volume(&self, samples: &mut [f32]) {
        apply_volume(samples, self.volume.load(Ordering::Relaxed));
    }

    pub fn clear(&self) {
        self.samples.lock().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        self.samples.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.lock().unwrap().is_empty()
    }
}

/// Microphone chain state: RNNoise, voice-activity gate, then the ring
/// songbird reads. The sound server hands us the stream spec directly —
/// 48 kHz mono f32 — so no downmix or resampling is needed here.
pub struct Capture {
    denoiser: Option<Box<DenoiseState<'static>>>,
    denoised: [f32; DenoiseState::FRAME_SIZE],
    frame: Vec<f32>,
    sensitivity: f32,
    input_volume: u8,
    open_until: Option<Instant>,
    /// AGC + compressor/limiter on what leaves the gate.
    dynamics: dynamics::Dynamics,
    ring: Arc<Ring>,
    /// Called every frame with whether we're speaking (voice.rs dedupes it
    /// into the green indicator).
    pub on_speaking: Option<Box<dyn FnMut(bool) + Send>>,
    last_log: Instant,
    peak: f32,
}

impl Capture {
    pub fn set_sensitivity(&mut self, sensitivity: u8) {
        self.sensitivity = f32::from(sensitivity) / 100.0;
    }

    pub fn set_volume(&mut self, percent: u8) {
        self.input_volume = percent.min(200);
    }

    pub fn set_dynamics(&mut self, auto_gain: bool, compressor: bool) {
        self.dynamics.agc = auto_gain;
        self.dynamics.compressor = compressor;
    }

    /// Toggling noise suppression (re)builds the RNNoise state.
    pub fn set_noise_suppression(&mut self, on: bool) {
        if on != self.denoiser.is_some() {
            self.denoiser = on.then(DenoiseState::new);
        }
    }
}

impl Capture {
    pub fn new(sensitivity: u8, noise_suppression: bool, ring: Arc<Ring>) -> Self {
        Self {
            denoiser: noise_suppression.then(DenoiseState::new),
            denoised: [0.0; DenoiseState::FRAME_SIZE],
            frame: Vec::with_capacity(DenoiseState::FRAME_SIZE),
            sensitivity: f32::from(sensitivity) / 100.0,
            input_volume: 100,
            open_until: None,
            dynamics: dynamics::Dynamics::default(),
            ring,
            on_speaking: None,
            last_log: Instant::now(),
            peak: 0.0,
        }
    }

    fn feed(&mut self, mono: &[f32]) {
        for &sample in mono {
            self.frame.push(sample);
            if self.frame.len() < DenoiseState::FRAME_SIZE {
                continue;
            }
            let mut frame = std::mem::replace(
                &mut self.frame,
                Vec::with_capacity(DenoiseState::FRAME_SIZE),
            );
            frame.resize(DenoiseState::FRAME_SIZE, 0.0);

            // nnnoiseless works on 16-bit-scaled floats and produces
            // silence for clipped input — limit hot signals into its range.
            let input_rms = rms(&frame);
            let denoised = self.denoiser.is_some();
            if denoised {
                let frame_rms = rms(&frame).max(0.001);
                let limiter = if frame_rms > 0.5 {
                    0.5 / frame_rms
                } else {
                    1.0
                };
                for sample in &mut frame {
                    *sample = (*sample * limiter).clamp(-0.95, 0.95) * 32768.0;
                }
            }
            let threshold =
                self.sensitivity.max(SPEAKING_FLOOR) * if denoised { 32768.0 } else { 1.0 };
            let loud = if let Some(state) = self.denoiser.as_mut() {
                state.process_frame(&mut self.denoised, &frame);
                rms(&self.denoised) >= threshold
            } else {
                rms(&frame) >= threshold
            };

            let now = Instant::now();
            self.peak = self.peak.max(input_rms);
            if self.last_log.elapsed() >= Duration::from_secs(1) {
                log::info!(
                    "mic: rms_pico={:.4} limiar={:.2} ring={}",
                    self.peak,
                    self.sensitivity,
                    self.ring.len()
                );
                self.peak = 0.0;
                self.last_log = now;
            }
            if loud {
                self.open_until = Some(now + VAD_HANGOVER);
            }
            let speaking = loud || self.open_until.is_some_and(|until| now < until);
            if let Some(notify) = self.on_speaking.as_mut() {
                notify(speaking);
            }
            let open = self.sensitivity <= 0.0 || speaking;
            if open {
                let mut out = if denoised {
                    self.denoised.iter().map(|s| s / 32768.0).collect()
                } else {
                    frame
                };
                self.dynamics.process(&mut out);
                apply_volume(&mut out, self.input_volume);
                self.ring.push(&out);
            } else {
                self.ring.clear();
            }
        }
    }
}

fn apply_volume(samples: &mut [f32], percent: u8) {
    let gain = f32::from(percent.min(200)) / 100.0;
    for sample in samples {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// The live microphone as songbird sees it: a `MediaSource` of 48 kHz mono
/// f32 PCM that yields silence when the ring is empty, so the mixer is never
/// blocked and never sees an EOF.
pub struct MicSource {
    ring: Arc<Ring>,
    scratch: Vec<f32>,
}

impl MicSource {
    pub fn new(ring: Arc<Ring>) -> Self {
        Self {
            ring,
            scratch: Vec::new(),
        }
    }
}

impl Read for MicSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Cap Symphonia's read-ahead at one tick. Pad starvation in one read
        // instead of making the decoder fetch hundreds of single samples.
        let want = (buf.len() / 4).min(VOICE_RATE as usize / 50);
        if want == 0 {
            return Ok(0);
        }
        self.scratch.resize(want, 0.0);
        let block = &mut self.scratch[..want];
        self.ring.pop(block);
        EFFECTS.add_send(block);
        for (i, sample) in block.iter().enumerate() {
            buf[i * 4..i * 4 + 4].copy_from_slice(&sample.to_le_bytes());
        }
        Ok(want * 4)
    }
}

impl Seek for MicSource {
    fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "o microfone não é procurável",
        ))
    }
}

impl MediaSource for MicSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_volume_scales_processed_microphone_without_changing_gate() {
        let ring = Ring::detached();
        let mut capture = Capture::new(30, false, ring.clone());
        let speaking = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indicator = speaking.clone();
        capture.on_speaking = Some(Box::new(move |value| {
            indicator.store(value, Ordering::Relaxed);
        }));
        for (volume, expected) in [(0, 0.0), (50, 0.2), (100, 0.4), (200, 0.8)] {
            capture.set_volume(volume);
            capture.feed(&vec![0.4; DenoiseState::FRAME_SIZE]);
            assert!(speaking.load(Ordering::Relaxed));
            let mut samples = vec![0.0; DenoiseState::FRAME_SIZE];
            ring.pop(&mut samples);
            assert!(
                samples
                    .iter()
                    .all(|sample| (*sample - expected).abs() < 0.0001)
            );
        }
    }

    #[test]
    fn output_volume_mutes_boosts_and_limits_the_final_mix() {
        let ring = Ring::new(16);
        for (volume, expected) in [
            (0, [0.0, 0.0]),
            (50, [0.4, -0.4]),
            (100, [0.8, -0.8]),
            (255, [1.0, -1.0]),
        ] {
            let mut mix = [0.8, -0.8];
            ring.set_volume(volume);
            ring.apply_volume(&mut mix);
            assert_eq!(mix, expected);
        }
    }

    #[test]
    fn microphone_reads_one_tick_and_pads_short_capture() {
        let ring = Ring::detached();
        ring.push(&vec![0.25; 480]);
        let mut mic = MicSource::new(ring.clone());
        let mut bytes = vec![0; 65_536];
        assert_eq!(mic.read(&mut bytes).unwrap(), 960 * 4);
        let samples: Vec<_> = bytes[..960 * 4]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert!(samples[..480].iter().all(|&s| s == 0.25));
        assert!(samples[480..].iter().all(|&s| s == 0.0));
        assert!(ring.is_empty());
        assert_eq!(mic.read(&mut bytes).unwrap(), 960 * 4);
    }

    #[test]
    fn microphone_does_not_read_ahead_more_than_twenty_ms() {
        let ring = Ring::detached();
        ring.push(&vec![0.25; 1920]);
        let mut mic = MicSource::new(ring.clone());
        assert_eq!(mic.read(&mut vec![0; 65_536]).unwrap(), 960 * 4);
        assert_eq!(ring.len(), 960);
    }
}
