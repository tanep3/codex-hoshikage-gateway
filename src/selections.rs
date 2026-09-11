//! Persistent, expiring selections bind Discord choices to one Proxy generation and scope.
use crate::{
    application::App,
    domain,
    proxy::{field, path_id},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::params;
use serde_json::{Value, json};

pub async fn save_menu(
    store: &Store,
    thread: &str,
    kind: &str,
    scope: &str,
    binding: String,
    items: Vec<Value>,
    cursor: Option<String>,
) -> Result<String> {
    ensure!(items.len() <= 25, "too many menu choices");
    let id = domain::id();
    let (token, t, k, s) = (
        id.clone(),
        thread.to_owned(),
        kind.to_owned(),
        scope.to_owned(),
    );
    store
        .call(true, move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM selection_menus WHERE expires_at<=?1",
                [domain::now_ms()],
            )?;
            let count: i64 =
                tx.query_row("SELECT count(*) FROM selection_menus", [], |r| r.get(0))?;
            ensure!(count < 1000, "too many active menus");
            tx.execute(
                "INSERT INTO selection_menus VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    token,
                    t,
                    k,
                    s,
                    binding,
                    serde_json::to_string(&items)?,
                    cursor,
                    domain::now_ms() + 600_000
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(id)
}
pub async fn load_menu(
    store: &Store,
    token: &str,
    thread: &str,
    binding: &str,
) -> Result<(String, String, Vec<Value>, Option<String>)> {
    let (id, t, b) = (token.to_owned(), thread.to_owned(), binding.to_owned());
    store.call(false,move|c|{
        let (kind,scope,items,cursor):(String,String,String,Option<String>)=c.query_row("SELECT kind,scope,items_json,next_cursor FROM selection_menus WHERE id=?1 AND thread_id=?2 AND binding_json=?3 AND expires_at>?4",params![id,t,b,domain::now_ms()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        Ok((kind,scope,serde_json::from_str(&items)?,cursor))
    }).await
}
impl App {
    pub(crate) async fn menu_binding(&self) -> Result<String> {
        let s = self.settings().await;
        ensure!(s.proxy.gate.is_ready(), "Proxy unavailable");
        let binding = s
            .proxy
            .v2
            .binding
            .read()
            .unwrap()
            .clone()
            .context("Proxy binding missing")?;
        Ok(serde_json::to_string(&binding)?)
    }
    pub(crate) async fn selection_page(
        &self,
        iid: &str,
        thread: &str,
        kind: &str,
        scope: &str,
        cursor: Option<&str>,
    ) -> Result<String> {
        self.authorized_thread(thread).await?;
        let binding = self.menu_binding().await?;
        let s = self.settings().await;
        let base = match kind {
            "workspace" => "/v2/codex/workspaces".to_owned(),
            "shared_artifact" => format!("/v2/codex/workspaces/{}/artifacts", path_id(scope)?),
            "artifact" => format!("/v2/codex/conversations/{}/artifacts", path_id(scope)?),
            _ => anyhow::bail!("unknown menu kind"),
        };
        // Use URL query encoding: cursors are opaque, not path fragments.
        let mut url = reqwest::Url::parse(&format!("http://local{base}"))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("limit", "25");
            if kind == "workspace" {
                q.append_pair("selectable", "true");
            }
            if let Some(c) = cursor {
                ensure!(c.len() <= 8192, "cursor too large");
                q.append_pair("cursor", c);
            }
        }
        let list = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("{}?{}", url.path(), url.query().unwrap_or("")),
                None,
                None,
            )
            .await?;
        ensure!(
            self.menu_binding().await? == binding,
            "Proxy changed during selection"
        );
        let data = list["data"].as_array().context("list missing")?;
        ensure!(data.len() <= 25, "Proxy exceeded page limit");
        let items: Vec<Value> = data
            .iter()
            .filter(|v| v["state"] == "ready")
            .cloned()
            .collect();
        let next = list["next_cursor"].as_str().map(str::to_owned);
        if items.is_empty() && next.is_none() {
            return Ok(if kind=="workspace"{"選べる共有ワークはありません。通常の会話には専用ワークが自動で用意されます。"}else{"このページに取得できる成果物はありません。/get の path 欄から作成済みファイルを指定することもできます。"}.into());
        }
        let token = save_menu(
            &self.store,
            thread,
            kind,
            scope,
            binding,
            items.clone(),
            next.clone(),
        )
        .await?;
        let mut rows = Vec::new();
        if !items.is_empty() {
            let options:Vec<Value>=items.iter().enumerate().map(|(n,v)|{
                let label=v["display_name"].as_str().unwrap_or(if kind=="workspace"{"共有ワーク"}else{"成果物"});
                let description=if kind=="workspace"{"他の会話とファイルを共有します".to_owned()}else{format!("版 {} / {} bytes / 作成 {} / 期限 {}",v["version"],v["size_bytes"],v["created_at"].as_str().unwrap_or("不明"),v["expires_at"].as_str().unwrap_or("不明"))};
                json!({"label":label.chars().take(100).collect::<String>(),"description":description.chars().take(100).collect::<String>(),"value":n.to_string()})
            }).collect();
            rows.push(json!({"type":1,"components":[{"type":3,"custom_id":format!("pick:{token}"),"placeholder":"選択してください（10分間有効）","options":options}]}));
        }
        if next.is_some() {
            rows.push(json!({"type":1,"components":[{"type":2,"style":2,"custom_id":format!("page:{token}"),"label":"次のページ"}]}));
        }
        let text = if kind == "workspace" {
            "共有ワークを選んでください。他の会話による変更も見える作業先です。通常は選択不要です。実行開始後は変更できません。"
        } else {
            "受け取る保存版を選んでください。選択画面は10分間有効です。"
        };
        self.delivery
            .text(
                &format!("selection-{iid}"),
                thread,
                "selection",
                0,
                text,
                json!(rows),
            )
            .await?;
        Ok("選択画面を表示しました。".into())
    }
    pub(crate) async fn selection_action(
        &self,
        iid: &str,
        thread: &str,
        custom: &str,
        value: Option<&str>,
    ) -> Result<String> {
        self.authorized_thread(thread).await?;
        let binding = self.menu_binding().await?;
        let (action, token) = custom.split_once(':').context("selection token missing")?;
        let Ok((kind, scope, items, cursor)) =
            load_menu(&self.store, token, thread, &binding).await
        else {
            return Ok("この選択画面は期限切れ、または接続先が変わっています。/get または /workspace から一覧を開き直してください。".into());
        };
        if action == "page" {
            return self
                .selection_page(
                    iid,
                    thread,
                    &kind,
                    &scope,
                    Some(cursor.as_deref().context("no next page")?),
                )
                .await;
        }
        ensure!(action == "pick", "invalid selection action");
        let index: usize = value.context("choice missing")?.parse()?;
        let item = items.get(index).context("choice outside menu")?;
        if kind == "shared_artifact" {
            return self
                .select_shared_artifact(iid, thread, &scope, &field(item, "artifact_id")?)
                .await;
        }
        if kind == "artifact" {
            let s = self.settings().await;
            let cv = s.proxy.ensure_conversation_v2(&self.store, thread).await?;
            ensure!(cv == scope, "conversation changed");
            return self
                .select_artifact(iid, thread, &field(item, "artifact_id")?)
                .await;
        }
        ensure!(kind == "workspace", "invalid selection kind");
        let _guard = self.config_mutation.lock().await;
        let s = self.settings().await;
        let workspace = field(item, "workspace_id")?;
        let meta = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/workspaces/{}", path_id(&workspace)?),
                None,
                None,
            )
            .await?;
        ensure!(
            meta["workspace_id"] == workspace && meta["state"] == "ready",
            "workspace unavailable"
        );
        ensure!(
            self.menu_binding().await? == binding,
            "Proxy changed during selection"
        );
        let t = thread.to_owned();
        let selected=self.store.call(true,move|c|{
            let tx=c.transaction()?;
            // A single INSERT races safely with automatic conversation creation. Neither can replace the winner.
            let model:String=tx.query_row("SELECT selected_model FROM conversations WHERE thread_id=?1",[&t],|r|r.get(0))?;
            ensure!(!model.is_empty(),"initial model missing");
            let changed=tx.execute("INSERT OR IGNORE INTO proxy_conversations(thread_id,request_key,request_json) VALUES(?1,?2,?3)",params![t,format!("conversation-{}",domain::id()),serde_json::to_string(&json!({"workspace":{"mode":"shared","workspace_id":workspace},"model":model}))?])?;
            tx.commit()?;Ok(changed==1)
        }).await?;
        Ok(if selected{"共有ワークを選びました。この会話の作業先として使います。"}else{"この会話の作業先はすでに確定しています。/new で新しい会話を作り、話しかける前に /workspace で選んでください。"}.into())
    }
}
