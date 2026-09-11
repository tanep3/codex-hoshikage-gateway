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
            ensure!(status.is_success(), "Discord HTTP {}", status.as_u16());
            return if bytes.is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_slice(&bytes).context("Discord JSON invalid")
            };
        }
        anyhow::bail!("Discord rate limit exhausted")
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
    pub async fn acknowledge(&self, id: &str, token: &str) -> Result<()> {
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
            Some(json!({"type":5,"data":{"flags":64}})),
        )
        .await?;
        Ok(())
    }
    pub async fn reply(&self, app: &str, token: &str, text: &str) -> Result<()> {
        ensure!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid interaction token"
        );
        self.api(
            reqwest::Method::PATCH,
            &format!("/webhooks/{}/{token}/messages/@original", snowflake(app)?),
            Some(json!({"content":text,"allowed_mentions":{"parse":[]}})),
        )
        .await?;
        Ok(())
    }
    pub async fn register(&self, app: &str, guild: &str) -> Result<()> {
        let me = self.get("/users/@me").await?;
        ensure!(me["bot"] == true, "Discord identity is not a bot");
        *self.bot_id.write().unwrap() =
            Some(me["id"].as_str().context("bot identity missing")?.into());
        let opt = |name: &str, description: &str, required: bool| json!({"type":3,"name":name,"description":description,"required":required});
        let commands = json!([
            {"name":"new","description":"このプロジェクトで会話を作成","options":[opt("title","会話の名前",true)]},
            {"name":"status","description":"実行・配信・会話の状態を表示"},
            {"name":"stop","description":"待ち行列を停止し、実行中の依頼へ中断を要求"},
            {"name":"resume","description":"一時停止した待ち行列の自動開始を再開"},
            {"name":"model","description":"次の依頼のモデルを選択／一覧表示","options":[opt("id","ProxyのモデルID",false)]},
            {"name":"steer","description":"現在のTurnへ追加指示","options":[opt("text","追加指示",true)]},
            {"name":"get","description":"指定した成果物を返送","options":[opt("path","プロジェクト相対パス",true)]}
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
        let data = json!({"content":"指定された成果物です。","nonce":nonce,"enforce_nonce":true,"allowed_mentions":{"parse":[]},"attachments":[{"id":0,"filename":filename}]});
        let form = reqwest::multipart::Form::new()
            .text("payload_json", serde_json::to_string(&data)?)
            .part(
                "files[0]",
                reqwest::multipart::Part::bytes(bytes).file_name(filename),
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
        ensure!(r.status().is_success(), "artifact delivery failed");
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
                serde_json::to_value(e.interaction)
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
