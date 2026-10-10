//! Microphone dynamics after the voice gate: automatic gain (AGC) toward a
//! speech level, a compressor that tames peaks, and a limiter that keeps
//! the result out of clipping.

use super::rms;

/// Speech level the AGC aims for: −20 dBFS RMS.
const AGC_TARGET: f32 = 0.1;
/// Below this RMS a frame is room noise: the AGC holds its gain instead of
/// pumping the noise floor up.
const AGC_FLOOR: f32 = 0.005;
/// AGC range: −12 dB to +18 dB.
const AGC_MIN_GAIN: f32 = 0.25;
const AGC_MAX_GAIN: f32 = 8.0;
/// Compressor: 4:1 above −12 dBFS.
const COMP_THRESHOLD: f32 = 0.25;
const COMP_RATIO: f32 = 4.0;
/// Envelope coefficients per sample at 48 kHz: 1 - e^(-1/(τ·fs)) for
/// τ = 1 ms (attack) and τ = 100 ms (release).
const COMP_ATTACK: f32 = 0.0206;
const COMP_RELEASE: f32 = 0.000_208;
/// Brick-wall ceiling (−0.4 dBFS).
const LIMIT: f32 = 0.95;

pub struct Dynamics {
    pub agc: bool,
    pub compressor: bool,
    gain: f32,
    envelope: f32,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self {
            agc: false,
            compressor: false,
            gain: 1.0,
            envelope: 0.0,
        }
    }
}

impl Dynamics {
    /// Processes one 10 ms mono frame in place.
    pub fn process(&mut self, frame: &mut [f32]) {
        if self.agc {
            let level = rms(frame);
            if level > AGC_FLOOR {
                let want = (AGC_TARGET / level).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
                // Quick to cut (no blasting), slow to boost (no pumping).
                let rate = if want < self.gain { 0.3 } else { 0.02 };
                self.gain += (want - self.gain) * rate;
            }
            for sample in frame.iter_mut() {
                *sample *= self.gain;
            }
        }
        if self.compressor {
            for sample in frame.iter_mut() {
                let level = sample.abs();
                let rate = if level > self.envelope {
                    COMP_ATTACK
                } else {
                    COMP_RELEASE
                };
                self.envelope += (level - self.envelope) * rate;
                if self.envelope > COMP_THRESHOLD {
                    let over = self.envelope / COMP_THRESHOLD;
                    *sample *= over.powf(1.0 / COMP_RATIO - 1.0);
                }
            }
        }
        if self.agc || self.compressor {
            for sample in frame.iter_mut() {
                *sample = sample.clamp(-LIMIT, LIMIT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32) -> Vec<f32> {
        (0..480)
            .map(|i| amplitude * (i as f32 * 0.13).sin())
            .collect()
    }

    #[test]
    fn agc_lifts_quiet_speech_and_compressor_tames_loud() {
        let mut agc = Dynamics {
            agc: true,
            ..Dynamics::default()
        };
        let mut frame = tone(0.02);
        for _ in 0..300 {
            frame = tone(0.02);
            agc.process(&mut frame);
        }
        assert!((rms(&frame) - AGC_TARGET).abs() < 0.02, "agc rms {}", rms(&frame));

        let mut comp = Dynamics {
            compressor: true,
            ..Dynamics::default()
        };
        let mut frame = tone(1.0);
        for _ in 0..20 {
            frame = tone(1.0);
            comp.process(&mut frame);
        }
        assert!(rms(&frame) < rms(&tone(1.0)) * 0.7);
        assert!(frame.iter().all(|s| s.abs() <= LIMIT));

        let mut off = Dynamics::default();
        let mut frame = tone(0.5);
        off.process(&mut frame);
        assert_eq!(frame, tone(0.5));
    }
}
