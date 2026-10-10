//! Thin Discord REST client (API v10) for the endpoints the native client
//! uses. User-account tokens go in raw; bot tokens may be passed with their
//! `Bot ` prefix and are forwarded as-is.

use serde::de::DeserializeOwned;

use crate::backend::soundboard::SoundboardSound;
use crate::model::{Channel, Guild, Member, Message, Relationship, User};

pub const REST_BASE: &str = "https://discord.com/api/v10";
pub const USER_AGENT: &str = concat!(
    "FastDiscord/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/FelipeMayerDev/FastDiscord)"
);

pub const MESSAGES_PER_PAGE: u64 = 50;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("token inválido ou expirado")]
    Unauthorized,
    #[error("rate limit do Discord, tente em {0}s")]
    RateLimited(u64),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        ApiError::Other(err.into())
    }
}

pub struct Api {
    http: reqwest::Client,
    token: String,
}

impl Api {
    pub fn new(token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build the HTTP client");
        Self {
            http,
            token: token.into(),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    async fn check_status(resp: reqwest::Response) -> ApiResult<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|value| value.get("retry_after").and_then(serde_json::Value::as_f64))
                .unwrap_or(1.0);
            return Err(ApiError::RateLimited(retry_after.ceil() as u64));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ApiError::Other(anyhow::anyhow!(
            "Discord API retornou {status}: {body}"
        )))
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> ApiResult<T> {
        let mut request = self
            .http
            .get(format!("{REST_BASE}{path}"))
            .header(reqwest::header::AUTHORIZATION, &self.token);
        if !query.is_empty() {
            request = request.query(query);
        }
        let resp = Self::check_status(request.send().await?).await?;
        Ok(resp.json::<T>().await?)
    }

    pub async fn gateway_url(&self) -> ApiResult<String> {
        #[derive(serde::Deserialize)]
        struct Gateway {
            url: String,
        }
        Ok(self.get_json::<Gateway>("/gateway", &[]).await?.url)
    }

    pub async fn me(&self) -> ApiResult<User> {
        self.get_json("/users/@me", &[]).await
    }

    pub async fn guilds(&self) -> ApiResult<Vec<Guild>> {
        self.get_json("/users/@me/guilds", &[]).await
    }

    pub async fn dm_channels(&self) -> ApiResult<Vec<Channel>> {
        self.get_json("/users/@me/channels", &[]).await
    }

    pub async fn relationships(&self) -> ApiResult<Vec<Relationship>> {
        self.get_json("/users/@me/relationships", &[]).await
    }

    pub async fn request_friend(&self, username: &str) -> ApiResult<()> {
        let resp = self
            .http
            .post(format!("{REST_BASE}/users/@me/relationships"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "username": username, "discriminator": null }))
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    pub async fn accept_friend(&self, user_id: &str) -> ApiResult<()> {
        let resp = self
            .http
            .put(format!("{REST_BASE}/users/@me/relationships/{user_id}"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "type": 1 }))
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    pub async fn remove_relationship(&self, user_id: &str) -> ApiResult<()> {
        let resp = self
            .http
            .delete(format!("{REST_BASE}/users/@me/relationships/{user_id}"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    pub async fn open_dm(&self, user_id: &str) -> ApiResult<Channel> {
        let resp = self
            .http
            .post(format!("{REST_BASE}/users/@me/channels"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "recipient_id": user_id }))
            .send()
            .await?;
        Ok(Self::check_status(resp).await?.json().await?)
    }

    pub async fn send_attachment(
        &self,
        channel_id: &str,
        content: &str,
        path: &std::path::Path,
    ) -> ApiResult<Message> {
        let (filename, bytes) = read_attachment(path)?;
        let payload = serde_json::json!({ "content": content, "attachments": [{ "id": 0, "filename": filename }] });
        let form = reqwest::multipart::Form::new()
            .text("payload_json", payload.to_string())
            .part(
                "files[0]",
                reqwest::multipart::Part::bytes(bytes).file_name(filename.to_owned()),
            );
        let resp = self
            .http
            .post(format!("{REST_BASE}/channels/{channel_id}/messages"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .multipart(form)
            .send()
            .await?;
        Ok(Self::check_status(resp).await?.json().await?)
    }

    pub async fn guild_channels(&self, guild_id: &str) -> ApiResult<Vec<Channel>> {
        self.get_json(&format!("/guilds/{guild_id}/channels"), &[])
            .await
    }

    pub async fn messages(
        &self,
        channel_id: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> ApiResult<Vec<Message>> {
        let mut query = vec![("limit", MESSAGES_PER_PAGE.to_string())];
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        // Discord returns newest first; the rest of the app (timeline,
        // gateway appends, the `before` cursor = first message) expects
        // chronological order.
        let mut messages: Vec<Message> = self
            .get_json(&format!("/channels/{channel_id}/messages"), &query)
            .await?;
        messages.sort_by(|a, b| (a.id.len(), &a.id).cmp(&(b.id.len(), &b.id)));
        Ok(messages)
    }

    pub async fn send_message(&self, channel_id: &str, content: &str) -> ApiResult<Message> {
        let resp = self
            .http
            .post(format!("{REST_BASE}/channels/{channel_id}/messages"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?;
        Ok(Self::check_status(resp).await?.json::<Message>().await?)
    }

    /// Fire-and-forget typing indicator; Discord replies with an empty 204.
    pub async fn send_typing(&self, channel_id: &str) -> ApiResult<()> {
        let resp = self
            .http
            .post(format!("{REST_BASE}/channels/{channel_id}/typing"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Discord's built-in sounds plus the guild's own (guild first, as in
    /// the official picker).
    pub async fn soundboard_sounds(&self, guild_id: &str) -> ApiResult<Vec<SoundboardSound>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<SoundboardSound>,
        }
        let mut sounds = self
            .get_json::<Items>(&format!("/guilds/{guild_id}/soundboard-sounds"), &[])
            .await?
            .items;
        sounds.extend(
            self.get_json::<Vec<SoundboardSound>>("/soundboard-default-sounds", &[])
                .await?,
        );
        Ok(sounds)
    }

    /// Plays a sound in the voice channel we're in; Discord answers 204 and
    /// broadcasts VOICE_CHANNEL_EFFECT_SEND.
    pub async fn send_soundboard_sound(
        &self,
        channel_id: &str,
        sound_id: &str,
        source_guild_id: Option<&str>,
    ) -> ApiResult<()> {
        let resp = self
            .http
            .post(format!(
                "{REST_BASE}/channels/{channel_id}/send-soundboard-sound"
            ))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({
                "sound_id": sound_id,
                "source_guild_id": source_guild_id,
            }))
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// One guild member; used to name users that voice states only
    /// reference by id (READY seeds carry no member payload).
    pub async fn guild_member(&self, guild_id: &str, user_id: &str) -> ApiResult<Member> {
        self.get_json(&format!("/guilds/{guild_id}/members/{user_id}"), &[])
            .await
    }
}

fn read_attachment(path: &std::path::Path) -> anyhow::Result<(String, Vec<u8>)> {
    use std::io::Read;
    const LIMIT: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::metadata(path)?;
    anyhow::ensure!(metadata.is_file(), "Escolha um arquivo regular");
    anyhow::ensure!(metadata.len() <= LIMIT, "Arquivo maior que 10 MB");
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Nome de arquivo inválido"))?
        .to_owned();
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "Escolha um arquivo regular");
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= LIMIT, "Arquivo maior que 10 MB");
    Ok((filename, bytes))
}

#[cfg(test)]
mod attachment_tests {
    use super::*;

    #[test]
    fn attachment_rejects_directories_and_large_files_and_preserves_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "fastdiscord-upload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        assert!(read_attachment(&dir).is_err());
        let path = dir.join("imagem.png");
        std::fs::write(&path, b"content").unwrap();
        assert_eq!(
            read_attachment(&path).unwrap(),
            ("imagem.png".into(), b"content".to_vec())
        );
        std::fs::File::create(&path)
            .unwrap()
            .set_len(10 * 1024 * 1024 + 1)
            .unwrap();
        assert!(read_attachment(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
