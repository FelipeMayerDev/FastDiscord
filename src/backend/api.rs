//! Thin Discord REST client (API v10) for the endpoints the native client
//! uses. User-account tokens go in raw; bot tokens may be passed with their
//! `Bot ` prefix and are forwarded as-is.

use serde::de::DeserializeOwned;

use crate::backend::soundboard::SoundboardSound;
use crate::model::{
    Channel, Gif, Guild, Member, Message, Profile, Relationship, ThreadList, ThreadSearch, User,
};

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
        let body = form_error(&body).unwrap_or(body);
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

    /// Up to a page of history centered on `message_id` (included unless
    /// it was deleted), in chronological order.
    pub async fn messages_around(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> ApiResult<Vec<Message>> {
        let query = [
            ("limit", MESSAGES_PER_PAGE.to_string()),
            ("around", message_id.to_string()),
        ];
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

    pub async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> ApiResult<Message> {
        let resp = self
            .http
            .patch(format!(
                "{REST_BASE}/channels/{channel_id}/messages/{message_id}"
            ))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?;
        Ok(Self::check_status(resp).await?.json::<Message>().await?)
    }

    pub async fn delete_message(&self, channel_id: &str, message_id: &str) -> ApiResult<()> {
        let resp = self
            .http
            .delete(format!(
                "{REST_BASE}/channels/{channel_id}/messages/{message_id}"
            ))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Stores status and custom status in the account settings, which the
    /// other clients apply on their next update or login. The legacy JSON
    /// route still maps onto the protobuf settings server-side.
    pub async fn save_status(
        &self,
        status: &str,
        custom: &crate::settings::CustomStatus,
    ) -> ApiResult<()> {
        let emoji = custom.emoji.trim();
        let custom_status = (!custom.is_empty()).then(|| {
            serde_json::json!({
                "text": custom.text.trim(),
                "emoji_name": (!emoji.is_empty()).then_some(emoji),
                "emoji_id": null,
                "expires_at": null,
            })
        });
        let resp = self
            .http
            .patch(format!("{REST_BASE}/users/@me/settings"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "status": status, "custom_status": custom_status }))
            .send()
            .await?;
        Self::check_status(resp).await?;
        Ok(())
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

    /// Archived public threads of a text, announcement or forum channel,
    /// newest first. Active ones come from the gateway.
    pub async fn archived_threads(&self, channel_id: &str) -> ApiResult<Vec<Channel>> {
        Ok(self
            .get_json::<ThreadList>(
                &format!("/channels/{channel_id}/threads/archived/public"),
                &[("limit", "50".to_string())],
            )
            .await?
            .into_threads())
    }

    /// A page of a forum's archived posts, as Discord's forum view asks for
    /// it (`sort_by` is `last_message_time` or `creation_time`).
    pub async fn search_forum_posts(
        &self,
        forum_id: &str,
        sort_by: &str,
        offset: usize,
    ) -> ApiResult<ThreadSearch> {
        // A forum not indexed yet answers 202 with no `threads`: an empty,
        // final page.
        self.get_json(
            &format!("/channels/{forum_id}/threads/search"),
            &[
                ("archived", "true".to_string()),
                ("sort_by", sort_by.to_string()),
                ("sort_order", "desc".to_string()),
                ("limit", "25".to_string()),
                ("tag_setting", "match_some".to_string()),
                ("offset", offset.to_string()),
            ],
        )
        .await
    }

    /// First messages of forum posts (`POST /channels/{forum}/post-data`),
    /// for the active ones the gateway delivers without them.
    pub async fn forum_post_data(
        &self,
        forum_id: &str,
        thread_ids: &[String],
    ) -> ApiResult<Vec<Message>> {
        #[derive(serde::Deserialize)]
        struct PostData {
            #[serde(default)]
            first_message: Option<Message>,
        }
        #[derive(serde::Deserialize)]
        struct Posts {
            threads: std::collections::HashMap<String, PostData>,
        }
        let resp = self
            .http
            .post(format!("{REST_BASE}/channels/{forum_id}/post-data"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(&serde_json::json!({ "thread_ids": thread_ids }))
            .send()
            .await?;
        let posts: Posts = Self::check_status(resp).await?.json().await?;
        Ok(posts
            .threads
            .into_values()
            .filter_map(|post| post.first_message)
            .collect())
    }

    /// Starts a thread from `message_id`, or a standalone thread / forum
    /// post when `None`; `body` is the endpoint's JSON.
    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: Option<&str>,
        body: &serde_json::Value,
    ) -> ApiResult<Channel> {
        let path = match message_id {
            Some(message_id) => format!("/channels/{channel_id}/messages/{message_id}/threads"),
            None => format!("/channels/{channel_id}/threads"),
        };
        let resp = self
            .http
            .post(format!("{REST_BASE}{path}"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(body)
            .send()
            .await?;
        Ok(Self::check_status(resp).await?.json().await?)
    }

    /// The official GIF picker's endpoints: trending for an empty query,
    /// search otherwise. `tinygif` makes `src` a small GIF the image cache
    /// can animate (the official client asks for mp4).
    pub async fn gifs(&self, query: &str) -> ApiResult<Vec<Gif>> {
        let mut params = vec![
            ("media_format", "tinygif".to_string()),
            ("provider", "tenor".to_string()),
            ("locale", "pt-BR".to_string()),
            ("limit", "30".to_string()),
        ];
        let path = if query.is_empty() {
            "/gifs/trending-gifs"
        } else {
            params.push(("q", query.to_string()));
            "/gifs/search"
        };
        self.get_json(path, &params).await
    }

    /// One guild member; used to name users that voice states only
    /// reference by id (READY seeds carry no member payload).
    pub async fn guild_member(&self, guild_id: &str, user_id: &str) -> ApiResult<Member> {
        self.get_json(&format!("/guilds/{guild_id}/members/{user_id}"), &[])
            .await
    }

    async fn patch(&self, path: &str, body: &serde_json::Value) -> ApiResult<reqwest::Response> {
        let resp = self
            .http
            .patch(format!("{REST_BASE}{path}"))
            .header(reqwest::header::AUTHORIZATION, &self.token)
            .json(body)
            .send()
            .await?;
        Self::check_status(resp).await
    }

    /// Our display name, pronouns, bio and, with `guild_id`, that guild's
    /// nick: GET /users/{id}/profile (`user_profile` + `guild_member`).
    pub async fn own_profile(&self, user_id: &str, guild_id: Option<&str>) -> ApiResult<Profile> {
        #[derive(serde::Deserialize)]
        struct Response {
            user: User,
            #[serde(default)]
            user_profile: Option<Meta>,
            #[serde(default)]
            guild_member: Option<Nick>,
        }
        #[derive(serde::Deserialize)]
        struct Meta {
            #[serde(default)]
            bio: Option<String>,
            #[serde(default)]
            pronouns: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Nick {
            #[serde(default)]
            nick: Option<String>,
        }
        let mut query = vec![("with_mutual_guilds", "false".to_string())];
        if let Some(guild_id) = guild_id {
            query.push(("guild_id", guild_id.to_string()));
        }
        let response: Response = self
            .get_json(&format!("/users/{user_id}/profile"), &query)
            .await?;
        let meta = response.user_profile;
        Ok(Profile {
            global_name: response.user.global_name.unwrap_or_default(),
            pronouns: meta
                .as_ref()
                .and_then(|meta| meta.pronouns.clone())
                .unwrap_or_default(),
            bio: meta.and_then(|meta| meta.bio).unwrap_or_default(),
            nick: response
                .guild_member
                .and_then(|member| member.nick)
                .unwrap_or_default(),
        })
    }

    /// PATCH /users/@me with `global_name` and/or `avatar` (a data URI);
    /// neither needs the password.
    pub async fn modify_me(&self, body: &serde_json::Value) -> ApiResult<User> {
        Ok(self.patch("/users/@me", body).await?.json().await?)
    }

    pub async fn modify_profile(&self, pronouns: &str, bio: &str) -> ApiResult<()> {
        let body = serde_json::json!({ "pronouns": pronouns, "bio": bio });
        self.patch("/users/@me/profile", &body).await?;
        Ok(())
    }

    /// Our nick in a guild; empty resets it to the display name.
    pub async fn modify_nick(&self, guild_id: &str, nick: &str) -> ApiResult<()> {
        let body = serde_json::json!({ "nick": (!nick.is_empty()).then_some(nick) });
        self.patch(&format!("/guilds/{guild_id}/members/@me"), &body)
            .await?;
        Ok(())
    }
}

/// The first field error of a 400 "Invalid Form Body", e.g.
/// `{"errors": {"avatar": {"_errors": [{"code": "AVATAR_RATE_LIMIT", …}]}}}`.
fn form_error(body: &str) -> Option<String> {
    fn first(value: &serde_json::Value) -> Option<&serde_json::Value> {
        if let Some(error) = value.get("_errors").and_then(|errors| errors.get(0)) {
            return Some(error);
        }
        value.as_object()?.values().find_map(first)
    }
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = first(value.get("errors")?)?;
    if error.get("code").and_then(serde_json::Value::as_str) == Some("AVATAR_RATE_LIMIT") {
        return Some("você trocou de avatar vezes demais; tente mais tarde".into());
    }
    error.get("message")?.as_str().map(str::to_owned)
}

pub(crate) fn read_attachment(path: &std::path::Path) -> anyhow::Result<(String, Vec<u8>)> {
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

    #[test]
    fn form_error_picks_the_nested_field_message() {
        let body = r#"{"code": 50035, "message": "Invalid Form Body", "errors": {"bio": {"_errors": [{"code": "BASE_TYPE_MAX_LENGTH", "message": "Must be 190 or fewer in length."}]}}}"#;
        assert_eq!(form_error(body).unwrap(), "Must be 190 or fewer in length.");
        assert!(form_error(r#"{"message": "Unknown"}"#).is_none());
        assert!(form_error("not json").is_none());
    }
}
