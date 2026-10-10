//! Soundboard (#12), as the official client does it: playing a sound is a
//! REST call, Discord broadcasts VOICE_CHANNEL_EFFECT_SEND to everyone in
//! the channel (us included) and every client plays the file locally.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Deserializer};

use super::audio::{EFFECTS, decode};

#[derive(Clone, Debug, Deserialize)]
pub struct SoundboardSound {
    #[serde(deserialize_with = "id_string")]
    pub sound_id: String,
    pub name: String,
    #[serde(default = "full_volume")]
    pub volume: f32,
    #[serde(default)]
    pub emoji_name: Option<String>,
    /// Set on guild sounds (the `source_guild_id` to send them with).
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default = "available")]
    pub available: bool,
}

fn full_volume() -> f32 {
    1.0
}

fn available() -> bool {
    true
}

/// Default sounds carry small integer ids, guild sounds snowflake strings.
pub fn id_string<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(de)? {
        serde_json::Value::String(id) => id,
        other => other.to_string(),
    })
}

/// Decoded sounds by id; they're short and repeat a lot.
static CACHE: LazyLock<Mutex<HashMap<String, Arc<[f32]>>>> = LazyLock::new(Mutex::default);

/// Downloads (once), decodes and plays a soundboard sound locally.
pub async fn play(sound_id: String, volume: f32) {
    let cached = CACHE.lock().unwrap().get(&sound_id).cloned();
    let clip = match cached {
        Some(clip) => clip,
        None => {
            let url = format!("https://cdn.discordapp.com/soundboard-sounds/{sound_id}");
            let bytes = match fetch(&url).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    log::warn!("soundboard: falha ao baixar {sound_id}: {err}");
                    return;
                }
            };
            let Ok(Some(clip)) = tokio::task::spawn_blocking(move || decode(bytes)).await else {
                log::warn!("soundboard: não consegui decodificar {sound_id}");
                return;
            };
            CACHE.lock().unwrap().insert(sound_id, Arc::clone(&clip));
            clip
        }
    };
    EFFECTS.play(clip, volume.clamp(0.0, 1.0));
}

async fn fetch(url: &str) -> reqwest::Result<Vec<u8>> {
    Ok(reqwest::get(url)
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_guild_sounds() {
        let sounds: Vec<SoundboardSound> = serde_json::from_str(
            r#"[{"name":"quack","sound_id":1,"volume":0.5,"emoji_name":"🦆","available":true},
                {"name":"x","sound_id":"1106714396018884649","guild_id":"9","emoji_id":null}]"#,
        )
        .unwrap();
        assert_eq!(sounds[0].sound_id, "1");
        assert_eq!(sounds[1].sound_id, "1106714396018884649");
        assert_eq!(sounds[1].volume, 1.0);
        assert_eq!(sounds[1].guild_id.as_deref(), Some("9"));
    }
}
