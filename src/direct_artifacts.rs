//! Per-conversation capture of an immutable file version. The call ID is
//! stable across repeated upstream events; paths and final answer prose are
//! never used to infer ownership across conversations.
use crate::{
    config::{Project, Workspace},
    direct_content::{DirectContent, StoredArtifact},
    domain, files,
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Artifact {
    pub id: String,
    pub request_id: Option<String>,
    pub thread_id: String,
    pub source_path: String,
    pub display_name: String,
    pub saved: StoredArtifact,
}

pub struct CaptureTarget<'a> {
    pub thread_id: &'a str,
    pub request_id: Option<&'a str>,
    pub call_id: &'a str,
    pub relative: &'a str,
    pub display_name: Option<&'a str>,
}

type ExistingRow = (String, Option<String>, String, String, String, String, i64);

fn name(input: &str) -> Result<String> {
    let value = input.trim();
    ensure!(
        !value.is_empty()
            && value.len() <= 120
            && !value.contains(['/', '\\'])
            && !value.chars().any(char::is_control),
        "artifact display name is invalid"
    );
    Ok(value.into())
}

pub async fn capture(
    store: &Store,
    content: &DirectContent,
    target: CaptureTarget<'_>,
    max_bytes: usize,
) -> Result<Artifact> {
    let CaptureTarget {
        thread_id,
        request_id,
        call_id,
        relative,
        display_name,
    } = target;
    ensure!(
        !call_id.is_empty() && call_id.len() <= 256,
        "artifact call ID is invalid"
    );
    ensure!(
        !relative.is_empty() && relative.len() <= 1024,
        "artifact path is invalid"
    );
    let display = name(display_name.unwrap_or_else(|| relative.rsplit('/').next().unwrap_or("")))?;
    let (lookup_thread, lookup_call) = (thread_id.to_owned(), call_id.to_owned());
    let previous = store
        .call(false, move |db| {
            Ok(db
                .query_row(
                    "SELECT id,request_id,discord_thread_id,source_path,display_name,relative_path,sha256,bytes
                     FROM direct_artifacts WHERE discord_thread_id=?1 AND call_id=?2",
                    params![lookup_thread, lookup_call],
                    |r| Ok(Artifact {
                        id:r.get(0)?,request_id:r.get(1)?,thread_id:r.get(2)?,source_path:r.get(3)?,
                        display_name:r.get(4)?,saved:StoredArtifact{
                            relative_path:r.get(5)?,sha256:r.get(6)?,bytes:r.get::<_,i64>(7)? as usize
                        }
                    }),
                )
                .optional()?)
        })
        .await?;
    if let Some(previous) = previous {
        ensure!(
            previous.request_id.as_deref() == request_id
                && previous.source_path == relative
                && previous.display_name == display,
            "artifact call ID changed its request or path"
        );
        content.read_artifact(&previous.saved, max_bytes)?;
        return Ok(previous);
    }
    if request_id.is_none() {
        let (thread, path) = (thread_id.to_owned(), relative.to_owned());
        let unresolved = store
            .call(false, move |db| {
                Ok(db
                    .prepare(
                        "SELECT 1 FROM direct_artifacts a JOIN deliveries d ON d.target_id=a.id
                         WHERE a.discord_thread_id=?1 AND a.source_path=?2
                           AND d.kind='direct-artifact' AND d.state='POST_PENDING' LIMIT 1",
                    )?
                    .exists(rusqlite::params![thread, path])?)
            })
            .await?;
        ensure!(
            !unresolved,
            "previous artifact delivery is still unconfirmed"
        );
    }
    let thread = thread_id.to_owned();
    let (path, dev, ino): (String, i64, i64) = store
        .call(false, move |db| {
            Ok(db.query_row(
                "SELECT workspace_path,workspace_dev,workspace_ino FROM direct_conversations WHERE discord_thread_id=?1",
                [&thread],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .await?;
    let path = PathBuf::from(path);
    let workspace = Workspace {
        project: Project {
            id: "direct".into(),
            name: "Direct conversation".into(),
            channel_id: thread_id.into(),
            cwd: path.clone(),
            default_model: String::new(),
            lifecycle: "ACTIVE".into(),
        },
        path,
        dev: dev as u64,
        ino: ino as u64,
    };
    let relative_owned = relative.to_owned();
    let bytes = tokio::task::spawn_blocking(move || {
        files::artifact(&workspace, &relative_owned, max_bytes)
    })
    .await
    .context("artifact capture worker failed")??;
    let key = domain::digest(serde_json::to_vec(&(thread_id, request_id, call_id))?.as_slice());
    let id = uuid::Uuid::from_u128(u128::from_str_radix(&key[..32], 16)?).to_string();
    let saved = content.save_artifact(&id, &bytes, max_bytes)?;
    let artifact = Artifact {
        id,
        request_id: request_id.map(str::to_owned),
        thread_id: thread_id.into(),
        source_path: relative.into(),
        display_name: display,
        saved,
    };
    let call_id = call_id.to_owned();
    let row = artifact.clone();
    store
        .call(true, move |db| {
            let tx = db.transaction()?;
            if let Some(request) = &row.request_id {
                let (bound, state): (String, String) = tx.query_row(
                    "SELECT thread_id,state FROM requests WHERE id=?1",
                    [request],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                ensure!(
                    bound == row.thread_id
                        && matches!(
                            state.as_str(),
                            "RUNNING" | "APPROVAL_REQUIRED" | "CANCEL_REQUESTED"
                        ),
                    "artifact does not belong to the live request"
                );
            }
            let existing: Option<ExistingRow> = tx
                .query_row(
                    "SELECT id,request_id,source_path,display_name,relative_path,sha256,bytes FROM direct_artifacts WHERE discord_thread_id=?1 AND call_id=?2",
                    params![row.thread_id,call_id],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
                )
                .optional()?;
            if let Some(existing) = existing {
                ensure!(
                    existing.0 == row.id
                        && existing.1 == row.request_id
                        && existing.2 == row.source_path
                        && existing.3 == row.display_name
                        && existing.4 == row.saved.relative_path
                        && existing.5 == row.saved.sha256
                        && existing.6 == row.saved.bytes as i64,
                    "artifact call ID changed content or destination"
                );
            } else {
                tx.execute(
                    "INSERT INTO direct_artifacts(id,request_id,discord_thread_id,call_id,source_path,display_name,relative_path,sha256,bytes,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'READY',?10)",
                    params![row.id,row.request_id,row.thread_id,call_id,row.source_path,row.display_name,row.saved.relative_path,row.saved.sha256,row.saved.bytes as i64,domain::now_ms()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(artifact)
}

pub async fn list(store: &Store, thread_id: &str) -> Result<Vec<Artifact>> {
    let thread = thread_id.to_owned();
    store.call(false, move |db| {
        let mut statement=db.prepare("SELECT id,request_id,discord_thread_id,source_path,display_name,relative_path,sha256,bytes FROM direct_artifacts WHERE discord_thread_id=?1 AND state='READY' ORDER BY created_at DESC,id DESC LIMIT 50")?;
        Ok(statement.query_map([thread],|r|Ok(Artifact{
            id:r.get(0)?,request_id:r.get(1)?,thread_id:r.get(2)?,source_path:r.get(3)?,display_name:r.get(4)?,
            saved:StoredArtifact{relative_path:r.get(5)?,sha256:r.get(6)?,bytes:r.get::<_,i64>(7)? as usize}
        }))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await
}
