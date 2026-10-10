//! Local sound effects mixed into the playback stream on top of the call
//! (#14 join/leave tones, #12 soundboard, #15 the DJ's local monitor), and
//! the DJ's send ring that `MicSource` mixes into what goes to the channel.
//! One global: there is one sound card and one voice connection.

use std::sync::{Arc, LazyLock, Mutex};

use super::{Ring, VOICE_RATE};

pub static EFFECTS: LazyLock<Effects> = LazyLock::new(|| Effects {
    clips: Mutex::new(Vec::new()),
    music: Ring::new(VOICE_RATE as usize * 2),
    send: Ring::new(VOICE_RATE as usize / 2),
});

/// A playing clip: stereo samples, read position, gain.
type Clip = (Arc<[f32]>, usize, f32);

pub struct Effects {
    clips: Mutex<Vec<Clip>>,
    /// DJ audio for our own speakers, stereo.
    pub music: Arc<Ring>,
    /// DJ audio for the channel, mono (the send mix is mono).
    pub send: Arc<Ring>,
}

impl Effects {
    pub fn play(&self, clip: Arc<[f32]>, gain: f32) {
        self.clips.lock().unwrap().push((clip, 0, gain));
    }

    /// Whether anything still has to reach the speakers (the output stays
    /// uncorked until it has, so the leave tone plays after a call ends).
    pub fn busy(&self) -> bool {
        !self.clips.lock().unwrap().is_empty() || !self.music.is_empty()
    }

    /// Adds clips and the DJ monitor into one stereo playback block.
    pub fn mix_into(&self, out: &mut [f32]) {
        let mut clips = self.clips.lock().unwrap();
        for (clip, pos, gain) in clips.iter_mut() {
            let take = (clip.len() - *pos).min(out.len());
            for (slot, sample) in out.iter_mut().zip(&clip[*pos..*pos + take]) {
                *slot += sample * *gain;
            }
            *pos += take;
        }
        clips.retain(|(clip, pos, _)| *pos < clip.len());
        drop(clips);
        if !self.music.is_empty() {
            let mut music = vec![0.0; out.len()];
            self.music.pop(&mut music);
            for (slot, sample) in out.iter_mut().zip(music) {
                *slot += sample;
            }
        }
    }

    /// Adds the DJ's pending send audio into a mono mic block.
    /// Called per mixer read (often a single sample), so no allocation.
    pub fn add_send(&self, block: &mut [f32]) {
        let mut send = self.send.samples.lock().unwrap();
        for slot in block.iter_mut() {
            let Some(sample) = send.pop_front() else {
                break;
            };
            *slot += sample;
        }
    }
}

/// Voice notification sounds from the official Discord client.
#[derive(Clone, Copy, Debug)]
pub enum Sound {
    Join,
    Leave,
    Disconnect,
    StreamStart,
    StreamStop,
}

pub fn play_sound(sound: Sound) {
    let index = match sound {
        Sound::Join => 0,
        Sound::Leave => 1,
        Sound::Disconnect => 2,
        Sound::StreamStart => 3,
        Sound::StreamStop => 4,
    };
    // The first MP3 decode must not stall the UI or the voice event loop.
    if let Err(error) = std::thread::Builder::new()
        .name("voice-sound".into())
        .spawn(move || {
            if let Some(clip) = &VOICE_SOUNDS[index] {
                EFFECTS.play(Arc::clone(clip), 0.5);
            }
        })
    {
        log::warn!("Não foi possível tocar som de voz: {error}");
    }
}

static VOICE_SOUNDS: LazyLock<[Option<Arc<[f32]>>; 5]> = LazyLock::new(|| {
    let assets: [&[u8]; 5] = [
        include_bytes!("../../../assets/sounds/user_join.mp3"),
        include_bytes!("../../../assets/sounds/user_leave.mp3"),
        include_bytes!("../../../assets/sounds/disconnect.mp3"),
        include_bytes!("../../../assets/sounds/stream_started.mp3"),
        include_bytes!("../../../assets/sounds/stream_ended.mp3"),
    ];
    assets.map(|bytes| decode(bytes.to_vec()))
});

/// Decodes an MP3/Ogg file (soundboard sounds) to 48 kHz stereo.
pub fn decode(bytes: Vec<u8>) -> Option<Arc<[f32]>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;
    let track = format.default_track()?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(VOICE_RATE);
    let mut stereo = Vec::new();
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else {
            continue;
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let left = frame[0];
            stereo.extend([left, frame.get(1).copied().unwrap_or(left)]);
        }
    }
    (!stereo.is_empty()).then(|| resample(&stereo, rate).into())
}

/// Linear-interpolation resample of stereo audio to 48 kHz.
// ponytail: linear interpolation aliases a little; fine for short effects.
fn resample(stereo: &[f32], rate: u32) -> Vec<f32> {
    if rate == VOICE_RATE || rate == 0 {
        return stereo.to_vec();
    }
    let frames = stereo.len() / 2;
    let out_frames = frames as u64 * u64::from(VOICE_RATE) / u64::from(rate);
    let step = rate as f64 / f64::from(VOICE_RATE);
    let mut out = Vec::with_capacity(out_frames as usize * 2);
    for i in 0..out_frames as usize {
        let pos = i as f64 * step;
        let at = (pos as usize).min(frames - 1);
        let next = (at + 1).min(frames - 1);
        let frac = (pos - at as f64) as f32;
        for channel in 0..2 {
            let a = stereo[at * 2 + channel];
            let b = stereo[next * 2 + channel];
            out.push(a + (b - a) * frac);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_clips_until_done_and_resamples() {
        let effects = Effects {
            clips: Mutex::new(Vec::new()),
            music: Ring::new(16),
            send: Ring::new(16),
        };
        effects.play(Arc::from(vec![0.5; 6]), 2.0);
        let mut block = [0.25; 4];
        effects.mix_into(&mut block);
        assert_eq!(block, [1.25; 4]);
        assert!(effects.busy());
        let mut block = [0.0; 4];
        effects.mix_into(&mut block);
        assert_eq!(block, [1.0, 1.0, 0.0, 0.0]);
        assert!(!effects.busy());

        // 24 kHz → 48 kHz doubles the frame count.
        assert_eq!(resample(&[0.0; 8], 24_000).len(), 16);
    }

    #[test]
    fn official_sounds_decode_to_finite_stereo_audio() {
        for clip in VOICE_SOUNDS.iter() {
            let clip = clip.as_ref().expect("bundled Discord sound must decode");
            assert!(!clip.is_empty());
            assert_eq!(clip.len() % 2, 0);
            assert!(clip.iter().all(|sample| sample.is_finite()));
            assert!(clip.iter().any(|sample| sample.abs() > 0.001));
        }
    }
}
