//! The DJ (#15): a queue of links or searches streamed through
//! `yt-dlp | ffmpeg` as 48 kHz stereo PCM — nothing is saved to disk — into
//! the channel (mixed with the mic, `EFFECTS.send`) and our own speakers
//! (`EFFECTS.music`). yt-dlp and ffmpeg are runtime dependencies.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::audio::EFFECTS;

/// Send audio kept queued ahead of the voice mixer: 100 ms of mono. The
/// mixer drains 20 ms per tick, which paces the whole pipe.
const AHEAD: usize = 48_000 / 10;

#[derive(Default)]
pub struct MusicState {
    pub queue: VecDeque<String>,
    /// Title (or the request, until yt-dlp names it) of what plays now.
    pub now: Option<String>,
    pub paused: bool,
    pub error: Option<String>,
    skip: bool,
    running: bool,
}

#[derive(Clone)]
pub struct Music {
    pub state: Arc<Mutex<MusicState>>,
    /// 0.0..=1.0, applied to both the channel and our speakers.
    pub volume: Arc<Mutex<f32>>,
}

impl Default for Music {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            volume: Arc::new(Mutex::new(0.5)),
        }
    }
}

impl Music {
    /// Queues a link, or a search when it isn't one.
    pub fn enqueue(&self, request: &str) {
        let request = request.trim();
        if request.is_empty() {
            return;
        }
        let target = if request.starts_with("http://") || request.starts_with("https://") {
            request.to_string()
        } else {
            format!("ytsearch1:{request}")
        };
        let mut state = self.state.lock().unwrap();
        state.queue.push_back(target);
        state.error = None;
        if !state.running {
            state.running = true;
            let music = self.clone();
            std::thread::Builder::new()
                .name("fastdiscord-dj".into())
                .spawn(move || music.run())
                .ok();
        }
    }

    pub fn skip(&self) {
        self.state.lock().unwrap().skip = true;
    }

    /// Empties the queue and ends the current track (left voice).
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.queue.clear();
        state.skip = true;
        EFFECTS.send.clear();
        EFFECTS.music.clear();
    }

    pub fn toggle_pause(&self) {
        let mut state = self.state.lock().unwrap();
        state.paused = !state.paused;
    }

    fn run(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().unwrap();
                state.skip = false;
                state.paused = false;
                let next = state.queue.pop_front();
                state.now = next.as_deref().map(label);
                if next.is_none() {
                    state.running = false;
                }
                next
            };
            let Some(target) = next else {
                return;
            };
            if let Err(error) = self.play(&target) {
                self.state.lock().unwrap().error = Some(error);
            }
        }
    }

    fn play(&self, target: &str) -> Result<(), String> {
        let missing = |tool: &str, err: std::io::Error| {
            if err.kind() == std::io::ErrorKind::NotFound {
                format!("{tool} não encontrado: instale yt-dlp e ffmpeg para usar o DJ")
            } else {
                format!("{tool}: {err}")
            }
        };
        // With `-o -`, yt-dlp prints everything (the title included) on
        // stderr, keeping stdout for the media.
        let mut ytdlp = Kill(
            hidden("yt-dlp")
                .args(["--no-playlist", "-f", "bestaudio/best", "--no-simulate"])
                .args(["--print", "title", "-q", "--no-warnings", "-o", "-", "--"])
                .arg(target)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| missing("yt-dlp", err))?,
        );
        let media = ytdlp.0.stdout.take().ok_or("yt-dlp sem saída")?;
        let mut ffmpeg = Kill(
            hidden("ffmpeg")
                .args(["-loglevel", "error", "-i", "pipe:0"])
                .args(["-f", "f32le", "-ac", "2", "-ar", "48000", "pipe:1"])
                .stdin(media)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| missing("ffmpeg", err))?,
        );
        let log = ytdlp.0.stderr.take();
        let state = Arc::clone(&self.state);
        let errors = std::thread::spawn(move || {
            let mut last_error = None;
            for line in BufReader::new(log?).lines().map_while(Result::ok) {
                if let Some(error) = line.strip_prefix("ERROR: ") {
                    last_error = Some(error.to_string());
                } else if !line.trim().is_empty() {
                    state.lock().unwrap().now = Some(line);
                }
            }
            last_error
        });

        let mut pcm = ffmpeg.0.stdout.take().ok_or("ffmpeg sem saída")?;
        // 20 ms of stereo f32.
        let mut chunk = vec![0u8; 960 * 2 * 4];
        let mut played = false;
        loop {
            {
                let state = self.state.lock().unwrap();
                if state.skip {
                    return Ok(());
                }
                if state.paused {
                    drop(state);
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
            if EFFECTS.send.len() > AHEAD {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            if pcm.read_exact(&mut chunk).is_err() {
                break;
            }
            played = true;
            let volume = *self.volume.lock().unwrap();
            let stereo: Vec<f32> = chunk
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes) * volume)
                .collect();
            let mono: Vec<f32> = stereo.chunks(2).map(|lr| (lr[0] + lr[1]) / 2.0).collect();
            EFFECTS.send.push(&mono);
            EFFECTS.music.push(&stereo);
        }
        drop(ffmpeg);
        drop(ytdlp);
        let error = errors.join().ok().flatten();
        match (played, error) {
            (false, Some(error)) => Err(error),
            (false, None) => Err("nada para tocar nesse link".into()),
            _ => Ok(()),
        }
    }
}

/// A queued target as the UI shows it (searches without their prefix).
pub fn label(target: &str) -> String {
    target.trim_start_matches("ytsearch1:").to_string()
}

/// Kills and reaps a child when the track ends or is skipped.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// No console window flashing up for the helpers on Windows.
fn hidden(program: &str) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}
