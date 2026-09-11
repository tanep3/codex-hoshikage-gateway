use crate::{
    application::App,
    domain,
    proxy::{field, path_id},
    selections::{load_menu, save_menu},
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use rusqlite::params;
use serde_json::{Value, json};
impl App {
    pub(crate) async fn shared_artifact_menu(&self, iid: &str, thread: &str) -> Result<String> {
        let s = self.settings().await;
        let cv = s.proxy.ensure_conversation_v2(&self.store, thread).await?;
        let cv = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/conversations/{}", path_id(&cv)?),
                None,
                None,
            )
            .await?;
        if cv["workspace_mode"] != "shared" {
            return Ok(
                "この会話は専用ワークを使っています。/get でこの会話の成果物を選べます。".into(),
            );
        }
        self.selection_page(
            iid,
            thread,
            "shared_artifact",
            &field(&cv, "workspace_id")?,
            None,
        )
        .await
    }
    pub(crate) async fn select_shared_artifact(
        &self,
        iid: &str,
        thread: &str,
        workspace: &str,
        id: &str,
    ) -> Result<String> {
        self.authorized_thread(thread).await?;
        let _guard = self.resource_mutation.write().await;
        let s = self.settings().await;
        let cv = s.proxy.ensure_conversation_v2(&self.store, thread).await?;
        let cv = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/conversations/{}", path_id(&cv)?),
                None,
                None,
            )
            .await?;
        ensure!(
            cv["workspace_mode"] == "shared" && cv["workspace_id"] == workspace,
            "shared workspace changed"
        );
        let meta = s
            .proxy
            .v2_json(
                Method::GET,
                &format!("/v2/codex/artifacts/{}", path_id(id)?),
                None,
                None,
            )
            .await?;
        ensure!(
            meta["artifact_id"] == id
                && meta["workspace_id"] == workspace
                && meta["state"] == "ready",
            "artifact outside shared scope"
        );
        self.queue_resource(iid, thread, "artifact", id).await?;
        let (i, ws) = (iid.to_owned(), workspace.to_owned());
        self.store.call(true,move|c|{c.execute("UPDATE resource_deliveries SET shared_workspace=?2 WHERE id=?1 AND state='WAITING'",params![i,ws])?;Ok(())}).await?;
        Ok("選んだ共有ワークの保存版を、この会話へ届けます。".into())
    }
    pub(crate) async fn retry_menu(&self, iid: &str, thread: &str) -> Result<String> {
        self.authorized_thread(thread).await?;
        let t = thread.to_owned();
        let items=self.store.call(false,move|c|{
            let mut st=c.prepare("SELECT id,resource_type,resource_id,coalesce(display_name,'回答テキスト'),state FROM resource_deliveries WHERE thread_id=?1 AND state IN ('WAITING','CACHED','POST_PENDING','BLOCKED','FAILED','EXPIRED') ORDER BY created_at DESC LIMIT 25")?;
            Ok(st.query_map([t],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"resource_id":r.get::<_,String>(2)?,"display_name":r.get::<_,String>(3)?,"state":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await?;
        if items.is_empty() {
            return Ok("再送待ちの回答・成果物はありません。成果物をもう一度受け取る場合は /get を使ってください。".into());
        }
        let token = save_menu(
            &self.store,
            thread,
            "retry",
            "delivery",
            self.menu_binding().await?,
            items.clone(),
            None,
        )
        .await?;
        let options:Vec<Value>=items.iter().enumerate().map(|(i,v)|json!({"label":v["display_name"].as_str().unwrap_or("保存版").chars().take(100).collect::<String>(),"description":format!("{} / {}",v["type"],v["state"]),"value":i.to_string()})).collect();
        self.delivery.text(&format!("retry-menu-{iid}"),thread,"retry_menu",0,"再送する保存版を選んでください（新しい順に最大25件）。選択後に確認画面が出ます。",json!([{"type":1,"components":[{"type":3,"custom_id":format!("retry:{token}"),"options":options}]}])).await?;
        Ok("再送候補を表示しました。AIの作業をやり直す操作ではありません。".into())
    }
    pub(crate) async fn retry_action(
        &self,
        iid: &str,
        thread: &str,
        custom: &str,
        value: Option<&str>,
    ) -> Result<String> {
        self.authorized_thread(thread).await?;
        let binding = self.menu_binding().await?;
        let (action, token) = custom.split_once(':').context("retry token missing")?;
        let Ok((kind, _, items, _)) = load_menu(&self.store, token, thread, &binding).await else {
            return Ok("選択画面の期限が切れています。/retry で開き直してください。".into());
        };
        if action == "retry" {
            ensure!(kind == "retry", "invalid retry menu");
            let index: usize = value.context("retry choice missing")?.parse()?;
            let item = items.get(index).context("retry choice invalid")?.clone();
            let confirm = save_menu(
                &self.store,
                thread,
                "resend",
                "delivery",
                binding,
                vec![item],
                None,
            )
            .await?;
            self.delivery.text(&format!("retry-confirm-{iid}"),thread,"retry_confirm",0,"同じ保存版をもう一度送ります。前の送信が成功していた場合、表示が重複することがあります。保存期限切れや権限撤回の場合は再送できません。",json!([{"type":1,"components":[{"type":2,"style":4,"label":"重複の可能性を承知して再送","custom_id":format!("resend:{confirm}")}]}])).await?;
            return Ok("確認ボタンで再送を開始します。".into());
        }
        ensure!(
            action == "resend" && kind == "resend",
            "invalid retry confirmation"
        );
        let _guard = self.resource_mutation.write().await;
        let original = field(items.first().context("retry target missing")?, "id")?;
        let (o, i, t) = (original.clone(), iid.to_owned(), thread.to_owned());
        let queued=self.store.call(true,move|c|{
            let tx=c.transaction()?;
            if tx.prepare("SELECT 1 FROM resource_deliveries WHERE id=?1")?.exists([&i])?{return Ok(true);}
            let n=tx.execute("INSERT INTO resource_deliveries(id,thread_id,resource_type,resource_id,request_key,display_name,sha256,size_bytes,created_at,retry_of,shared_workspace) SELECT ?1,thread_id,resource_type,resource_id,?2,display_name,sha256,size_bytes,?3,id,shared_workspace FROM resource_deliveries WHERE id=?4 AND thread_id=?5 AND state IN ('WAITING','CACHED','POST_PENDING','BLOCKED','FAILED','EXPIRED')",params![i,format!("lease-{i}"),domain::now_ms(),o,t])?;
            if n==1{tx.execute("UPDATE resource_deliveries SET state='SUPERSEDED' WHERE id=?1",[&o])?;}
            tx.commit()?;Ok(n==1)
        }).await?;
        if queued {
            self.output.lock().await.remove(&original);
            Ok("同じ保存版の再送を受け付けました。AIは再実行しません。".into())
        } else {
            Ok("対象は配信済み、またはすでに別の再送へ切り替わっています。/status で確認してください。".into())
        }
    }
}
