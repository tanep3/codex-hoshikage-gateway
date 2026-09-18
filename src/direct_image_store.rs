//! Persistent image inventory tied to an exact direct Codex Turn.
use crate::{codex_execution::TurnIdentity, direct_content::StoredImage, domain, storage::Store};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageRecordState {
    Ready(StoredImage),
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRecord {
    pub item_id: String,
    pub ordinal: usize,
    pub state: ImageRecordState,
}
struct ExistingImage {
    ordinal: i64,
    state: String,
    path: Option<String>,
    sha256: Option<String>,
    bytes: Option<i64>,
}

impl Store {
    pub async fn direct_images_to_deliver(&self, limit: usize) -> Result<Vec<(String, String)>> {
        ensure!(
            (1..=100).contains(&limit),
            "invalid image recovery page size"
        );
        self.call(false,move|connection|{
            let mut stmt=connection.prepare("SELECT DISTINCT r.id,r.thread_id FROM direct_generated_images i JOIN requests r ON r.id=i.request_id LEFT JOIN deliveries d ON d.target_id=r.id AND d.kind='direct-image' AND d.part=i.ordinal WHERE i.state='READY' AND r.state IN ('COMPLETED','FAILED','CANCELLED') AND (d.id IS NULL OR d.state!='CONFIRMED') ORDER BY r.updated_at LIMIT ?1")?;
            Ok(stmt.query_map([limit as i64],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }
    pub async fn mark_direct_image_unknown(
        &self,
        request_id: String,
        identity: TurnIdentity,
        reason: &'static str,
    ) -> Result<()> {
        self.call(true,move|connection|{
            let (thread,turn):(Option<String>,Option<String>)=connection.query_row("SELECT codex_thread_id,codex_turn_id FROM direct_dispatches WHERE request_id=?1",[&request_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
            ensure!(thread.as_deref()==Some(identity.thread_id.as_str()) && turn.as_deref()==Some(identity.turn_id.as_str()),"image inventory belongs to another Turn");
            connection.execute("INSERT INTO direct_image_inventories(request_id,state,error_code,updated_at) VALUES(?1,'UNKNOWN',?2,?3) ON CONFLICT(request_id) DO UPDATE SET state='UNKNOWN',error_code=excluded.error_code,updated_at=excluded.updated_at WHERE direct_image_inventories.state='UNKNOWN'",params![request_id,reason,domain::now_ms()])?;
            Ok(())
        }).await
    }
    pub async fn record_direct_images(
        &self,
        request_id: String,
        identity: TurnIdentity,
        images: Vec<ImageRecord>,
    ) -> Result<()> {
        self.call(true, move |connection| {
            let tx=connection.transaction()?;
            let (thread,turn,send_state):(Option<String>,Option<String>,String)=tx.query_row(
                "SELECT codex_thread_id,codex_turn_id,send_state FROM direct_dispatches WHERE request_id=?1",
                [&request_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
            ensure!(thread.as_deref()==Some(identity.thread_id.as_str()) && turn.as_deref()==Some(identity.turn_id.as_str()) && matches!(send_state.as_str(),"ACKED"|"UNKNOWN"|"TERMINAL"),"image inventory belongs to another Turn");
            let now=domain::now_ms();
            let complete=images.iter().all(|image|image.state!=ImageRecordState::Unknown);
            let previous:Option<String>=tx.query_row("SELECT state FROM direct_image_inventories WHERE request_id=?1",[&request_id],|row|row.get(0)).optional()?;
            if previous.as_deref()==Some("COMPLETE") {
                let count:i64=tx.query_row("SELECT count(*) FROM direct_generated_images WHERE request_id=?1",[&request_id],|row|row.get(0))?;
                ensure!(complete && count==images.len() as i64,"completed image inventory changed");
            }
            tx.execute("INSERT INTO direct_image_inventories(request_id,state,updated_at) VALUES(?1,?2,?3) ON CONFLICT(request_id) DO UPDATE SET state=excluded.state,updated_at=excluded.updated_at WHERE direct_image_inventories.state='UNKNOWN'",params![request_id,if complete{"COMPLETE"}else{"UNKNOWN"},now])?;
            for image in &images {
                ensure!(!image.item_id.is_empty() && image.item_id.len()<=256,"invalid image item identity");
                let (state,path,sha,bytes):(String,Option<String>,Option<String>,Option<i64>)=match &image.state {
                    ImageRecordState::Ready(saved)=>("READY".into(),Some(saved.relative_path.clone()),Some(saved.sha256.clone()),Some(saved.bytes as i64)),
                    ImageRecordState::Failed=>("FAILED".into(),None,None,None),
                    ImageRecordState::Unknown=>("UNKNOWN".into(),None,None,None),
                };
                let existing:Option<ExistingImage>=tx.query_row(
                    "SELECT ordinal,state,relative_path,sha256,bytes FROM direct_generated_images WHERE request_id=?1 AND item_id=?2",
                    params![request_id,image.item_id],|row|Ok(ExistingImage{ordinal:row.get(0)?,state:row.get(1)?,path:row.get(2)?,sha256:row.get(3)?,bytes:row.get(4)?}),
                ).optional()?;
                if let Some(prior)=existing {
                    ensure!(prior.ordinal==image.ordinal as i64,"image order changed");
                    if prior.state=="UNKNOWN" && state!="UNKNOWN" {
                        tx.execute("UPDATE direct_generated_images SET state=?3,relative_path=?4,sha256=?5,bytes=?6,updated_at=?7 WHERE request_id=?1 AND item_id=?2 AND state='UNKNOWN'",params![request_id,image.item_id,state,path,sha,bytes,now])?;
                    } else {
                        ensure!(prior.state==state && prior.path==path && prior.sha256==sha && prior.bytes==bytes,"saved image identity changed");
                    }
                } else {
                    tx.execute("INSERT INTO direct_generated_images(request_id,item_id,ordinal,state,relative_path,sha256,bytes,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![request_id,image.item_id,image.ordinal as i64,state,path,sha,bytes,now])?;
                }
            }
            if complete {
                let count:i64=tx.query_row("SELECT count(*) FROM direct_generated_images WHERE request_id=?1",[&request_id],|row|row.get(0))?;
                ensure!(count==images.len() as i64,"image inventory item count changed");
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn direct_image_inventory(
        &self,
        request_id: String,
    ) -> Result<Option<(String, Vec<ImageRecord>)>> {
        self.call(true,move|connection|{
            let state:Option<String>=connection.query_row("SELECT state FROM direct_image_inventories WHERE request_id=?1",[&request_id],|row|row.get(0)).optional()?;
            let Some(state)=state else{return Ok(None)};
            let mut stmt=connection.prepare("SELECT item_id,ordinal,state,relative_path,sha256,bytes FROM direct_generated_images WHERE request_id=?1 ORDER BY ordinal")?;
            let rows=stmt.query_map([&request_id],|row|{
                let status:String=row.get(2)?;
                let value=match status.as_str() {
                    "READY"=>ImageRecordState::Ready(StoredImage{relative_path:row.get(3)?,sha256:row.get(4)?,bytes:row.get::<_,i64>(5)? as usize}),
                    "FAILED"=>ImageRecordState::Failed,
                    _=>ImageRecordState::Unknown,
                };
                Ok(ImageRecord{item_id:row.get(0)?,ordinal:row.get::<_,i64>(1)? as usize,state:value})
            })?;
            Ok(Some((state,rows.collect::<rusqlite::Result<Vec<_>>>()?)))
        }).await
    }
}
