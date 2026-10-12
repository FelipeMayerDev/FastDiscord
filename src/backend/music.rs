//! Local DJ playback, paced against the shared session clock, never the mic.
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::audio::EFFECTS;
use super::dj::{Clock, Session, Track};

#[derive(Default)]
pub struct MusicState {
    pub error: Option<String>,
    pub loading: bool,
    downloaded: bool,
    disabled: bool,
    generation: u64,
    playback: Option<(Session, Clock)>,
    children: Vec<Arc<Mutex<Child>>>,
}
#[derive(Clone)]
pub struct Music {
    pub state: Arc<Mutex<MusicState>>,
}
impl Default for Music {
    fn default() -> Self {
        Self {
            state: Arc::default(),
        }
    }
}
impl Music {
    pub fn set_volume(&self, percent: u8) {
        EFFECTS.music.set_volume(percent);
    }
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        if state.disabled {
            return;
        }
        cancel(&mut state);
        state.playback = None;
        state.loading = false;
    }
    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.disabled = true;
        cancel(&mut state);
        state.playback = None;
        state.loading = false;
    }
    pub fn apply(&self, session: &Session, clock: &Clock) {
        let mut state = self.state.lock().unwrap();
        if state.disabled {
            return;
        }
        let changed = state.playback.as_ref().is_none_or(|(old, _)| {
            old.epoch != session.epoch || old.current() != session.current()
        });
        let hold_changed = state.playback.as_ref().is_some_and(|(old, _)| {
            old.paused != session.paused || old.preparing != session.preparing
        });
        if changed {
            cancel(&mut state);
            state.error = None;
            state.downloaded = false;
        }
        if hold_changed {
            EFFECTS.music.clear();
        }
        state.playback = session.current().map(|_| (session.clone(), clock.clone()));
        if !changed || session.current().is_none() {
            state.loading = session.current().is_some() && state.loading;
            return;
        }
        state.loading = true;
        let generation = state.generation;
        let track = session.current().unwrap().clone();
        let music = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("fastdiscord-dj".into())
            .spawn(move || {
                let result = music.play(&track, generation);
                let mut state = music.state.lock().unwrap();
                if state.generation == generation {
                    state.loading = false;
                    if let Err(error) = result {
                        state.error = Some(error);
                    }
                }
            })
        {
            state.loading = false;
            state.error = Some(format!("Não foi possível iniciar o DJ: {error}"));
        }
    }
    pub fn preparation(&self) -> Option<(u64, bool)> {
        let state = self.state.lock().unwrap();
        let (session, _) = state.playback.as_ref()?;
        if state.downloaded {
            Some((session.epoch, true))
        } else if state.error.is_some() {
            Some((session.epoch, false))
        } else {
            None
        }
    }
    fn register_child(&self, child: Child, generation: u64) -> Result<Kill, String> {
        let child = Kill(Arc::new(Mutex::new(child)));
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err("Reprodução cancelada".into());
        }
        state.children.push(child.0.clone());
        Ok(child)
    }
    fn play(&self, track: &Track, generation: u64) -> Result<(), String> {
        let temporary = TemporaryTrack::new()?;
        let media = temporary.0.join("audio.media");
        let ytdlp = self.register_child(
            hidden("yt-dlp")
                .args([
                    "--ignore-config",
                    "--no-playlist",
                    "--socket-timeout",
                    "10",
                    "--retries",
                    "2",
                    "--max-filesize",
                    "512M",
                    "-f",
                    "bestaudio/best",
                    "-q",
                    "--no-warnings",
                    "-o",
                ])
                .arg(&media)
                .arg("--")
                .arg(track.url())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(tool_error("yt-dlp"))?,
            generation,
        )?;
        let log = ytdlp
            .0
            .lock()
            .unwrap()
            .stderr
            .take()
            .ok_or("yt-dlp sem diagnóstico")?;
        let errors = std::thread::Builder::new()
            .name("dj-errors".into())
            .spawn(move || {
                let mut last_error = None;
                for line in BufReader::new(log).lines().map_while(Result::ok) {
                    if let Some(error) = line.strip_prefix("ERROR: ") {
                        last_error = Some(error.chars().take(500).collect::<String>());
                    }
                }
                last_error
            })
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        let download = loop {
            if self.state.lock().unwrap().generation != generation {
                break Err("Reprodução cancelada".into());
            }
            if let Some(status) = ytdlp
                .0
                .lock()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
            {
                break if status.success() && media.metadata().is_ok_and(|m| m.len() > 0) {
                    Ok(())
                } else {
                    Err("Não foi possível baixar a música completa".into())
                };
            }
            if started.elapsed() > Duration::from_secs(300) {
                break Err("Download demorou demais; tente outra música".into());
            }
            let size: u64 = std::fs::read_dir(&temporary.0)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.metadata().ok())
                .map(|m| m.len())
                .sum();
            if size > 512 * 1024 * 1024 {
                break Err("A música excede o limite temporário de 512 MiB".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        drop(ytdlp);
        let error = errors.join().ok().flatten();
        download.map_err(|fallback| error.unwrap_or(fallback))?;
        let offset = {
            let state = self.state.lock().unwrap();
            if state.generation != generation {
                return Ok(());
            }
            state
                .playback
                .as_ref()
                .map_or(0, |(session, clock)| session.position(clock.now()))
        };
        let ffmpeg = self.register_child(
            hidden("ffmpeg")
                .args(["-nostdin", "-loglevel", "error", "-ss"])
                .arg(format!("{:.3}", offset as f64 / 1000.0))
                .arg("-i")
                .arg(&media)
                .args(["-f", "f32le", "-ac", "2", "-ar", "48000", "pipe:1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(tool_error("ffmpeg"))?,
            generation,
        )?;
        let mut pcm = ffmpeg
            .0
            .lock()
            .unwrap()
            .stdout
            .take()
            .ok_or("ffmpeg sem saída")?;
        let mut chunk = [0u8; 960 * 2 * 4];
        pcm.read_exact(&mut chunk)
            .map_err(|_| "Não foi possível decodificar a música baixada")?;
        {
            let mut state = self.state.lock().unwrap();
            if state.generation != generation {
                return Ok(());
            }
            state.downloaded = true;
        }
        let mut first_block = true;
        let mut frame_ms = offset;
        let mut played = false;
        loop {
            let timing = {
                let state = self.state.lock().unwrap();
                if state.generation != generation {
                    break;
                }
                state.playback.as_ref().map(|(session, clock)| {
                    (
                        session.paused || session.preparing,
                        clock.now() < session.start_at_ms,
                        session.position(clock.now()),
                    )
                })
            };
            let Some((paused, scheduled, target)) = timing else {
                break;
            };
            if paused || scheduled || frame_ms > target + 40 || EFFECTS.music.len() > 3840 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            if first_block {
                first_block = false;
            } else if pcm.read_exact(&mut chunk).is_err() {
                break;
            }
            frame_ms += 20;
            if frame_ms + 40 < target {
                continue;
            }
            let stereo: Vec<f32> = chunk
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect();
            let mut state = self.state.lock().unwrap();
            if state.generation != generation {
                break;
            }
            if state.playback.as_ref().is_none_or(|(session, clock)| {
                session.paused
                    || session.preparing
                    || clock.now() < session.start_at_ms
                    || frame_ms + 40 < session.position(clock.now())
            }) {
                continue;
            }
            EFFECTS.music.push(&stereo);
            state.loading = false;
            played = true;
        }
        drop(ffmpeg);
        if self.state.lock().unwrap().generation != generation {
            return Ok(());
        }
        if !played {
            return Err("Não foi possível carregar o áudio desta música".into());
        }
        Ok(())
    }
}
struct TemporaryTrack(std::path::PathBuf);
impl TemporaryTrack {
    fn new() -> Result<Self, String> {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fastdiscord-dj-{}-{suffix}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|e| format!("Não foi possível criar o temporário do DJ: {e}"))?;
        Ok(Self(path))
    }
}
impl Drop for TemporaryTrack {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            log::warn!("Não foi possível remover temporário do DJ: {error}");
        }
    }
}

fn tool_error(tool: &'static str) -> impl FnOnce(std::io::Error) -> String {
    move |error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{tool} não encontrado: instale yt-dlp e ffmpeg para usar o DJ")
        } else {
            format!("{tool}: {error}")
        }
    }
}
fn cancel(state: &mut MusicState) {
    state.generation += 1;
    for child in state.children.drain(..) {
        kill_child(&mut child.lock().unwrap());
    }
    EFFECTS.music.clear();
}
struct Kill(Arc<Mutex<Child>>);
impl Drop for Kill {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap();
        kill_child(&mut child);
        let _ = child.wait();
    }
}

/// Resolve once before sharing; everyone receives the same video id, never a search.
pub fn resolve(request: &str) -> Result<Track, String> {
    if request.is_empty() || request.len() > 2048 {
        return Err("Link ou busca inválidos".into());
    }
    let target = if request.contains("://") {
        let url = reqwest::Url::parse(request).map_err(|_| "Link inválido")?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some(
                    "youtube.com"
                        | "www.youtube.com"
                        | "m.youtube.com"
                        | "music.youtube.com"
                        | "youtu.be"
                )
            )
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err("Use uma busca ou um link HTTPS do YouTube".into());
        }
        request.to_string()
    } else {
        return Err("Pesquise e selecione uma música antes de adicionar".into());
    };
    parse_track(&metadata(&target, false)?)
}

pub fn search(query: &str) -> Result<Vec<Track>, String> {
    let query = query.trim();
    if query.is_empty() || query.len() > 2048 || query.contains("://") {
        return Err("Busca inválida".into());
    }
    parse_search_results(&metadata(&format!("ytsearch10:{query}"), true)?)
}

fn parse_search_results(json: &serde_json::Value) -> Result<Vec<Track>, String> {
    let entries = json
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or("Resposta de busca inválida")?;
    let mut tracks = Vec::new();
    for entry in entries.iter().take(10) {
        if let Ok(track) = parse_track(entry)
            && !tracks
                .iter()
                .any(|old: &Track| old.video_id == track.video_id)
        {
            tracks.push(track);
        }
    }
    if tracks.is_empty() {
        return Err("Nenhuma música com duração válida encontrada; tente outra busca".into());
    }
    Ok(tracks)
}

fn metadata(target: &str, flat: bool) -> Result<serde_json::Value, String> {
    let mut command = hidden("yt-dlp");
    if flat {
        command.arg("--flat-playlist");
    }
    let child = command
        .args([
            "--ignore-config",
            "--no-playlist",
            "--skip-download",
            "--dump-single-json",
            "--socket-timeout",
            "10",
            "--retries",
            "1",
            "--no-warnings",
            "--",
        ])
        .arg(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(tool_error("yt-dlp"))?;
    let guard = Kill(Arc::new(Mutex::new(child)));
    let stdout = guard
        .0
        .lock()
        .unwrap()
        .stdout
        .take()
        .ok_or("yt-dlp sem saída")?;
    let child = guard.0.clone();
    let reader = std::thread::spawn(move || {
        let mut result = Vec::new();
        let read = stdout.take(8 * 1024 * 1024 + 1).read_to_end(&mut result);
        (read, result)
    });
    let started = Instant::now();
    loop {
        if child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            break;
        }
        if started.elapsed() > Duration::from_secs(45) {
            drop(guard);
            let _ = reader.join();
            return Err("A busca da música demorou demais; tente novamente".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (result, bytes) = reader.join().map_err(|_| "Falha na busca da música")?;
    result.map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Resposta da música grande demais".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "Música não encontrada ou indisponível no YouTube".into())
}
fn parse_track(json: &serde_json::Value) -> Result<Track, String> {
    let json = json
        .get("entries")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
        .unwrap_or(json);
    if json
        .get("is_live")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err("Lives não têm uma duração fixa para sincronizar".into());
    }
    let duration = json
        .get("duration")
        .and_then(|v| v.as_f64())
        .filter(|d| d.is_finite() && *d > 0.0 && *d <= 86_400.0)
        .ok_or("Música sem duração válida")?;
    let track = Track {
        video_id: json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        title: json
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into(),
        duration_ms: (duration * 1000.0) as u64,
    };
    track.validate()?;
    Ok(track)
}
/// No console window flashing up for the helpers on Windows.
fn hidden(program: &str) -> Command {
    #[cfg(windows)]
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| bundled_tool(program, &exe));
    #[cfg(windows)]
    let mut command = Command::new(bundled.as_deref().unwrap_or(std::path::Path::new(program)));
    #[cfg(not(windows))]
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        if program == "yt-dlp" {
            configure_bundled_downloader(&mut command, bundled.as_deref());
        }
    }
    command
}

#[cfg(any(windows, test))]
fn bundled_tool(program: &str, exe: &std::path::Path) -> Option<std::path::PathBuf> {
    if !matches!(program, "yt-dlp" | "ffmpeg" | "ffprobe" | "deno") {
        return None;
    }
    let directory = exe.parent()?;
    let name = format!("{program}.exe");
    [directory.join("tools").join(&name), directory.join(name)]
        .into_iter()
        .find(|path| path.is_file())
}

#[cfg(any(windows, test))]
fn configure_bundled_downloader(command: &mut Command, bundled: Option<&std::path::Path>) {
    let Some(directory) = bundled.and_then(std::path::Path::parent) else {
        return;
    };
    command.args(["--ignore-config", "--no-update", "--no-remote-components"]);
    if directory.join("ffmpeg.exe").is_file() {
        command.arg("--ffmpeg-location").arg(directory);
    }
    let deno = directory.join("deno.exe");
    if deno.is_file() {
        let mut runtime = std::ffi::OsString::from("deno:");
        runtime.push(deno);
        command.arg("--js-runtimes").arg(runtime);
    }
}

fn kill_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // SAFETY: hidden() gives each helper its own process group, so the
        // negative pid targets only that helper and its download children.
        unsafe { kill(-(child.id() as i32), 9) };
    }
    #[cfg(windows)]
    {
        let _ = hidden("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

#[cfg(test)]
mod bundled_tool_tests {
    use super::*;

    #[test]
    fn bundled_helpers_resolve_without_path_and_keep_unicode_paths_literal() {
        let directory = std::env::temp_dir().join(format!(
            "fastdiscord DJ ç {}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).unwrap();
        let exe = directory.join("fastdiscord.exe");
        assert!(bundled_tool("yt-dlp", &exe).is_none());
        // Source/custom builds can also place a helper directly beside the app.
        std::fs::write(directory.join("yt-dlp.exe"), []).unwrap();
        assert_eq!(
            bundled_tool("yt-dlp", &exe),
            Some(directory.join("yt-dlp.exe"))
        );
        let tools = directory.join("tools");
        std::fs::create_dir(&tools).unwrap();
        for program in ["yt-dlp", "ffmpeg", "ffprobe", "deno"] {
            std::fs::write(tools.join(format!("{program}.exe")), []).unwrap();
            assert_eq!(
                bundled_tool(program, &exe),
                Some(tools.join(format!("{program}.exe")))
            );
        }
        assert!(bundled_tool("taskkill", &exe).is_none());
        let mut command = Command::new("yt-dlp");
        configure_bundled_downloader(&mut command, bundled_tool("yt-dlp", &exe).as_deref());
        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(args[4], tools.as_os_str());
        let mut deno = std::ffi::OsString::from("deno:");
        deno.push(tools.join("deno.exe"));
        assert_eq!(args[6], deno);
        let mut fallback = Command::new("yt-dlp");
        configure_bundled_downloader(&mut fallback, None);
        assert_eq!(fallback.get_args().count(), 0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_resolves_one_video_and_rejects_live_or_invalid_media() {
        let json = serde_json::json!({"entries": [{"id": "abcdefghijk", "title": "Music", "duration": 12.5}]});
        assert_eq!(parse_track(&json).unwrap().duration_ms, 12500);
        assert!(parse_track(&serde_json::json!({"id": "abcdefghijk", "title": "Live", "duration": 10, "is_live": true})).is_err());
        assert!(
            parse_track(&serde_json::json!({"id": "../evil", "title": "Invalid", "duration": 10}))
                .is_err()
        );
        assert!(resolve("https://127.0.0.1/private").is_err());
        assert!(resolve("https://youtube.com.evil.test/watch").is_err());
    }
    #[test]
    fn search_returns_choices_and_skips_invalid_entries() {
        let json = serde_json::json!({"entries": [
            {"id":"abcdefghijk", "title":"First", "duration":120},
            {"id":"lmnopqrstuv", "title":"Second", "duration":140},
            {"id":"abcdefghijk", "title":"Duplicate", "duration":120},
            {"id":"12345678901", "title":"Live", "duration":140, "is_live":true},
            null
        ]});
        let tracks = parse_search_results(&json).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].title, "Second");
        assert!(parse_search_results(&serde_json::json!({"entries":[]})).is_err());
        assert!(resolve("ordinary search text").is_err());
    }
    #[test]
    fn temporary_media_is_removed_on_drop_including_partial_downloads() {
        let path;
        {
            let temporary = TemporaryTrack::new().unwrap();
            path = temporary.0.clone();
            std::fs::write(path.join("audio.media.part"), b"partial").unwrap();
        }
        assert!(!path.exists());
    }
    #[test]
    fn disconnected_music_cannot_be_restarted_by_a_stale_task() {
        let music = Music::default();
        music.shutdown();
        let generation = music.state.lock().unwrap().generation;
        music.apply(&Session::default(), &Clock::new(0));
        music.stop();
        let state = music.state.lock().unwrap();
        assert_eq!(state.generation, generation);
        assert!(state.playback.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn stop_cancels_blocked_reads_and_rejects_stale_workers() {
        let music = Music::default();
        let child = hidden("sh")
            .args(["-c", "sleep 30 & printf ready; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let guard = music.register_child(child, 0).unwrap();
        let mut stdout = guard.0.lock().unwrap().stdout.take().unwrap();
        let mut ready = [0; 5];
        stdout.read_exact(&mut ready).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || tx.send(stdout.read(&mut [0; 4])).unwrap());
        music.stop();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap(), 0);
        reader.join().unwrap();
        let child = hidden("sleep").arg("30").spawn().unwrap();
        assert!(music.register_child(child, 0).is_err());
        assert!(music.state.lock().unwrap().children.is_empty());
    }
}
