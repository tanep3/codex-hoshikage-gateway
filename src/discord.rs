use crate::files::{Attachment, InputMessage};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use serenity::{
    async_trait,
    client::{Context as DiscordContext, RawEventHandler},
    model::event::Event,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct HttpStatus(pub u16);
impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Discord HTTP {}", self.0)
    }
}
impl std::error::Error for HttpStatus {}

#[derive(Clone)]
pub struct Discord {
    client: reqwest::Client,
    token: Arc<String>,
    base: Arc<String>,
    bot_id: Arc<std::sync::RwLock<Option<String>>>,
}
impl Discord {
    pub fn new(token: String) -> Result<Self> {
        Self::with_endpoint(token, "https://discord.com/api/v10".into())
    }
    /// Transport endpoint injection for isolated integration tests; runtime uses new().
    pub fn with_endpoint(token: String, base: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            token: Arc::new(token),
            base: Arc::new(base),
            bot_id: Arc::new(std::sync::RwLock::new(None)),
        })
    }
    pub fn owns_message(&self, v: &Value) -> bool {
        self.bot_id
            .read()
            .unwrap()
            .as_deref()
            .is_some_and(|id| v["author"]["id"].as_str() == Some(id))
    }
    pub fn should_respond(&self, message: &Value, mode: crate::config::ResponseMode) -> bool {
        if mode == crate::config::ResponseMode::All {
            return true;
        }
        let bot = self.bot_id.read().unwrap();
        let Some(bot) = bot.as_deref() else {
            return false;
        };
        message["mentions"]
            .as_array()
            .is_some_and(|mentions| mentions.iter().any(|user| user["id"].as_str() == Some(bot)))
    }
    pub fn secret(&self) -> String {
        (*self.token).clone()
    }
    pub async fn api(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("Authorization", format!("Bot {}", self.token));
        if let Some(body) = body {
            request = request.json(&body);
        }
        for attempt in 0..3 {
            let response = request
                .try_clone()
                .context("Discord request not replayable")?
                .send()
                .await
                .context("Discord request result unavailable")?;
            let status = response.status();
            let mut stream = response.bytes_stream();
            let mut bytes = vec![];
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("Discord body unavailable")?;
                ensure!(
                    bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                    "Discord response too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            if status.as_u16() == 429 && attempt < 2 {
                // A complete Discord 429 explicitly rejects execution. Only this response permits
                // rate-limit retry; transport loss, redirects and 5xx never enter this path.
                let v: Value =
                    serde_json::from_slice(&bytes).context("rate limit response invalid")?;
                let delay = v["retry_after"]
                    .as_f64()
                    .context("rate limit delay missing")?;
                ensure!(
                    delay.is_finite() && (0.0..=60.0).contains(&delay),
                    "rate limit delay unsupported"
                );
                tokio::time::sleep(Duration::from_secs_f64(delay.max(0.05))).await;
                continue;
            }
            if !status.is_success() {
                return Err(HttpStatus(status.as_u16()).into());
            }
            return if bytes.is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_slice(&bytes).context("Discord JSON invalid")
            };
        }
        anyhow::bail!("Discord rate limit exhausted")
    }
    pub async fn remove_own_message(&self, thread: &str, message: &str) -> Result<bool> {
        let url = format!(
            "{}/channels/{}/messages/{}",
            self.base,
            snowflake(thread)?,
            snowflake(message)?
        );
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.token.as_str()))
            .send()
            .await?;
        if response.status().as_u16() == 404 {
            return Ok(true);
        }
        ensure!(
            response.status().is_success(),
            "surplus message lookup failed"
        );
        let value: Value = response.json().await?;
        ensure!(
            value["id"] == message && value["channel_id"] == thread && self.owns_message(&value),
            "surplus message ownership mismatch"
        );
        let response = self
            .client
            .delete(url)
            .header("Authorization", format!("Bot {}", self.token.as_str()))
            .send()
            .await?;
        Ok(response.status().is_success() || response.status().as_u16() == 404)
    }
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.api(reqwest::Method::GET, path, None).await
    }
    pub async fn message(&self, thread: &str, id: &str, guild: &str) -> Result<InputMessage> {
        let v = self
            .get(&format!(
                "/channels/{}/messages/{}",
                snowflake(thread)?,
                snowflake(id)?
            ))
            .await?;
        ensure!(
            v["id"] == id
                && v["channel_id"] == thread
                && v["author"]["bot"] != true
                && v["webhook_id"].is_null(),
            "message identity invalid"
        );
        input_message(&v, guild)
    }
    pub async fn interaction_callback(&self, id: &str, token: &str, body: Value) -> Result<()> {
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(
            reqwest::Method::POST,
            &format!("/interactions/{}/{token}/callback", snowflake(id)?),
            Some(body),
        )
        .await?;
        Ok(())
    }
    pub async fn followup_components(
        &self,
        app: &str,
        token: &str,
        text: &str,
        components: Value,
    ) -> Result<()> {
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(reqwest::Method::POST,&format!("/webhooks/{}/{token}",snowflake(app)?),Some(json!({"content":text,"flags":64,"components":components,"allowed_mentions":{"parse":[]}}))).await?;
        Ok(())
    }
    pub async fn acknowledge(&self, id: &str, token: &str) -> Result<()> {
        self.acknowledge_kind(id, token, false).await
    }
    pub async fn acknowledge_update(&self, id: &str, token: &str) -> Result<()> {
        self.acknowledge_kind(id, token, true).await
    }
    async fn acknowledge_kind(&self, id: &str, token: &str, update: bool) -> Result<()> {
        // Interaction tokens are deliberately kept only in memory.
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(
            reqwest::Method::POST,
            &format!("/interactions/{}/{token}/callback", snowflake(id)?),
            Some(if update {
                json!({"type":6})
            } else {
                json!({"type":5,"data":{"flags":64}})
            }),
        )
        .await?;
        Ok(())
    }
    pub async fn followup_error(&self, app: &str, token: &str, text: &str) -> Result<()> {
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(
            reqwest::Method::POST,
            &format!("/webhooks/{}/{token}", snowflake(app)?),
            Some(json!({"content":text,"flags":64,"allowed_mentions":{"parse":[]}})),
        )
        .await?;
        Ok(())
    }
    pub async fn reply(&self, app: &str, token: &str, text: &str) -> Result<()> {
        self.reply_components(app, token, text, json!([])).await
    }
    pub async fn reply_components(
        &self,
        app: &str,
        token: &str,
        text: &str,
        components: Value,
    ) -> Result<()> {
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(
            reqwest::Method::PATCH,
            &format!("/webhooks/{}/{token}/messages/@original", snowflake(app)?),
            Some(json!({"content":text,"components":components,"allowed_mentions":{"parse":[]}})),
        )
        .await?;
        Ok(())
    }
    pub async fn check_attachment_permissions(&self, thread: &str, guild: &str) -> Result<()> {
        let bot = self
            .bot_id
            .read()
            .unwrap()
            .clone()
            .context("Bot identity missing")?;
        let mut channel = self
            .get(&format!("/channels/{}", snowflake(thread)?))
            .await?;
        ensure!(
            channel["id"] == thread && channel["guild_id"] == guild,
            "attachment channel mismatch"
        );
        let in_thread = matches!(channel["type"].as_u64(), Some(10..=12));
        if in_thread {
            let parent = channel["parent_id"]
                .as_str()
                .context("thread parent missing")?
                .to_owned();
            channel = self
                .get(&format!("/channels/{}", snowflake(&parent)?))
                .await?;
            ensure!(
                channel["id"] == parent && channel["guild_id"] == guild,
                "attachment parent mismatch"
            );
        }
        let member = self
            .get(&format!(
                "/guilds/{}/members/{}",
                snowflake(guild)?,
                snowflake(&bot)?
            ))
            .await?;
        let roles = self
            .get(&format!("/guilds/{}/roles", snowflake(guild)?))
            .await?;
        if !crate::discord_permissions::attachment_allowed(
            guild,
            &bot,
            &roles,
            &member,
            &channel["permission_overwrites"],
            in_thread,
        )? {
            return Err(crate::proxy_v2::ApiError {
                status: 403,
                code: "discord_permission_denied".into(),
                retry: "none".into(),
            }
            .into());
        }
        Ok(())
    }
    pub async fn identify_bot(&self) -> Result<()> {
        let me = self.get("/users/@me").await?;
        ensure!(me["bot"] == true, "Discord identity is not a bot");
        *self.bot_id.write().unwrap() =
            Some(me["id"].as_str().context("bot identity missing")?.into());
        Ok(())
    }
    pub async fn register(&self, app: &str, guild: &str) -> Result<()> {
        self.identify_bot().await?;
        let opt = |name: &str, description: &str, required: bool| json!({"type":3,"name":name,"description":description,"required":required});
        let commands = json!([
            {"name":"new","description":"新しい会話を作成","options":[opt("title","会話の名前",true)]},
            {"name":"mcp","description":"この作業のMCP許可を確認・取消"},
            {"name":"status","description":"実行・配信・会話の状態を表示"},
            {"name":"stop","description":"待ち行列を停止し、実行中の依頼へ中断を要求"},
            {"name":"cancel","description":"直近の待機依頼1件を取消。待機がなければ実行中の依頼を中断"},
            {"name":"resume","description":"一時停止した待ち行列の自動開始を再開"},
            {"name":"models","description":"利用可能なモデルを一覧表示"},
            {"name":"model","description":"選択中モデルの確認／次の依頼のモデル変更","options":[opt("id","ProxyのモデルID",false)]},
            {"name":"steer","description":"現在のTurnへ追加指示","options":[opt("text","追加指示",true)]},
            {"name":"workspace","description":"会話を始める前に共有ワークを選択（通常は不要）"},
            {"name":"retry","description":"保存版を選び、確認してもう一度送信"},
            {"name":"get","description":"指定した成果物を返送","options":[opt("path","ワーク内の相対パス（省略で一覧）",false),{"type":3,"name":"scope","description":"成果物一覧の範囲","required":false,"choices":[{"name":"この会話","value":"conversation"},{"name":"共有ワーク全体","value":"shared"}]}]}
        ]);
        self.api(
            reqwest::Method::PUT,
            &format!(
                "/applications/{}/guilds/{}/commands",
                snowflake(app)?,
                snowflake(guild)?
            ),
            Some(commands),
        )
        .await?;
        Ok(())
    }
    /// Direct runtime commands are registered as one complete guild command
    /// set. Keeping them separate prevents legacy Proxy-only commands from
    /// appearing on a direct instance.
    pub async fn register_direct(&self, app: &str, guild: &str) -> Result<()> {
        self.identify_bot().await?;
        let option = |name: &str, description: &str, required: bool| json!({"type":3,"name":name,"description":description,"required":required});
        let commands = json!([
            {"name":"status","description":"この会話の実行・待機状態を確認"},
            {"name":"stop","description":"実行中の依頼を中断し、待機列を一時停止"},
            {"name":"cancel","description":"直近の待機依頼を取り消し。なければ実行中を中断"},
            {"name":"resume","description":"一時停止した待機列を再開"},
            {"name":"models","description":"Codexで利用できるモデル一覧"},
            {"name":"model","description":"選択中モデルの確認・変更","options":[option("id","モデルID",false)]},
            {"name":"get","description":"この会話の成果物一覧、またはファイルの保存版を返送","options":[option("path","会話ワーク内の相対パス（省略で一覧）",false)]},
            {"name":"steer","description":"実行中の依頼に追加指示","options":[option("text","追加指示",true)]}
        ]);
        self.api(
            reqwest::Method::PUT,
            &format!(
                "/applications/{}/guilds/{}/commands",
                snowflake(app)?,
                snowflake(guild)?
            ),
            Some(commands),
        )
        .await?;
        Ok(())
    }
    pub async fn upload(
        &self,
        thread: &str,
        filename: String,
        bytes: Vec<u8>,
        nonce: &str,
    ) -> Result<Value> {
        self.upload_attachment(thread, filename, bytes, nonce, None)
            .await
    }
    pub async fn upload_attachment(
        &self,
        thread: &str,
        filename: String,
        bytes: Vec<u8>,
        nonce: &str,
        image: Option<(String, String)>,
    ) -> Result<Value> {
        let mut data = json!({"content":"指定された成果物です。","nonce":nonce,"enforce_nonce":true,"allowed_mentions":{"parse":[]},"attachments":[{"id":0,"filename":filename}]});
        let mime = if let Some((message, caption)) = image {
            snowflake(&message)?;
            data["content"] = json!(caption);
            data["message_reference"] = json!({"message_id":message,"fail_if_not_exists":false});
            data["allowed_mentions"]["replied_user"] = json!(false);
            "image/png"
        } else {
            "application/octet-stream"
        };
        let form = reqwest::multipart::Form::new()
            .text("payload_json", serde_json::to_string(&data)?)
            .part(
                "files[0]",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(mime)?,
            );
        let r = self
            .client
            .post(format!(
                "{}/channels/{}/messages",
                self.base,
                snowflake(thread)?
            ))
            .header("Authorization", format!("Bot {}", self.token))
            .multipart(form)
            .send()
            .await
            .context("artifact delivery unknown")?;
        if !r.status().is_success() {
            let status = r.status().as_u16();
            let code = match status {
                413 => "discord_attachment_too_large",
                403 => "discord_permission_denied",
                400 => "discord_attachment_rejected",
                _ => "discord_delivery_unconfirmed",
            };
            return Err(crate::proxy_v2::ApiError {
                status,
                code: code.into(),
                retry: "none".into(),
            }
            .into());
        }
        r.json().await.context("artifact receipt unknown")
    }
}
pub fn snowflake(s: &str) -> Result<&str> {
    ensure!(
        !s.is_empty()
            && s.len() <= 20
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u64>()? > 0,
        "invalid Discord ID"
    );
    Ok(s)
}
pub fn input_message(v: &Value, guild: &str) -> Result<InputMessage> {
    let attachments = v["attachments"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|a| {
                    Ok(Attachment {
                        id: a["id"].as_str().context("attachment ID missing")?.into(),
                        filename: a["filename"]
                            .as_str()
                            .context("attachment name missing")?
                            .into(),
                        url: a["url"].as_str().context("attachment URL missing")?.into(),
                        size: a["size"].as_u64().context("attachment size missing")?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    // Hash the opaque edit timestamp, avoiding timezone/parser differences between gateway and REST.
    let edited_at = v["edited_timestamp"].as_str().map(|s| {
        let h = crate::domain::digest(s.as_bytes());
        i64::from_str_radix(&h[..15], 16).unwrap()
    });
    Ok(InputMessage {
        id: v["id"].as_str().context("message ID missing")?.into(),
        thread_id: v["channel_id"]
            .as_str()
            .context("channel ID missing")?
            .into(),
        user_id: v["author"]["id"]
            .as_str()
            .context("author ID missing")?
            .into(),
        guild_id: guild.into(),
        content: v["content"].as_str().unwrap_or("").into(),
        edited_at,
        attachments,
    })
}
/// Serenity serializes the variant body without the wire discriminator.
/// Preserve it at the ingress boundary for buttons and modal submissions.
pub fn interaction_value(interaction: &serenity::model::application::Interaction) -> Result<Value> {
    let mut value = serde_json::to_value(interaction)?;
    value["type"] = serde_json::to_value(interaction.kind())?;
    Ok(value)
}
pub enum Incoming {
    Message(Value),
    Interaction(Value),
    Connected(String),
    Reconnected,
}
pub struct Handler {
    pub messages: mpsc::Sender<Incoming>,
    pub controls: mpsc::Sender<Incoming>,
    pub cancel: CancellationToken,
}
#[async_trait]
impl RawEventHandler for Handler {
    async fn raw_event(&self, _ctx: DiscordContext, event: Event) {
        let (sender, value) = match event {
            Event::MessageCreate(e) => (
                &self.messages,
                serde_json::to_value(e.message).ok().map(Incoming::Message),
            ),
            Event::InteractionCreate(e) => (
                &self.controls,
                interaction_value(&e.interaction)
                    .ok()
                    .map(Incoming::Interaction),
            ),
            Event::Resumed(_) => (&self.controls, Some(Incoming::Reconnected)),
            Event::Ready(e) => (
                &self.controls,
                Some(Incoming::Connected(e.ready.application.id.to_string())),
            ),
            _ => return,
        };
        if let Some(value) = value
            && sender.try_send(value).is_err()
        {
            self.cancel.cancel();
        }
    }
}

pub struct Health {
    pub connected: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait]
impl serenity::client::EventHandler for Health {
    async fn shard_stage_update(
        &self,
        _ctx: DiscordContext,
        event: serenity::gateway::ShardStageUpdateEvent,
    ) {
        if event.new != serenity::gateway::ConnectionStage::Connected {
            self.connected
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod response_mode_tests {
    use super::*;
    use crate::config::ResponseMode;

    #[test]
    fn mention_mode_uses_bot_identity_and_structured_mentions() {
        let discord = Discord::new("test-only".into()).unwrap();
        assert!(discord.should_respond(&json!({}), ResponseMode::All));
        assert!(!discord.should_respond(&json!({"mentions":[{"id":"42"}]}), ResponseMode::Mention));
        *discord.bot_id.write().unwrap() = Some("42".into());
        assert!(discord.should_respond(&json!({"mentions":[{"id":"42"}]}), ResponseMode::Mention));
        assert!(!discord.should_respond(
            &json!({"content":"<@42>","mentions":[{"id":"99"}]}),
            ResponseMode::Mention
        ));
        assert!(!discord.should_respond(&json!({"mentions":[]}), ResponseMode::Mention));
    }

    #[test]
    fn response_mode_defaults_and_rejects_typos() {
        let base = r#"guild_id = "1"
allowed_user_id = "2"
token_file = "/tmp/unused"
"#;
        let config: crate::config::Discord = toml::from_str(base).unwrap();
        assert_eq!(config.response_mode, ResponseMode::All);
        let config: crate::config::Discord =
            toml::from_str(&format!("{base}response_mode = \"mention\"\n")).unwrap();
        assert_eq!(config.response_mode, ResponseMode::Mention);
        assert!(
            toml::from_str::<crate::config::Discord>(&format!(
                "{base}response_mode = \"mentions\"\n"
            ))
            .is_err()
        );
    }
}
