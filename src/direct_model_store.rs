//! Discord model selection is ordered and idempotent; catalog validation is
//! performed outside the SQLite transaction by `DirectModelCatalog`.
use crate::{domain, storage::Store};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub async fn select_direct_model(
        &self,
        thread: String,
        interaction: String,
        model: String,
    ) -> Result<bool> {
        let order = interaction.parse::<i64>()?;
        ensure!(
            order > 0 && model.len() <= 128 && !model.is_empty(),
            "invalid model selection identity"
        );
        self.call(true,move|connection|{
            let tx=connection.transaction()?;
            let mode:String=tx.query_row("SELECT mode FROM runtime_mode WHERE singleton=1",[],|row|row.get(0))?;
            ensure!(mode=="direct","direct model selection requires direct runtime");
            let previous:Option<(String,String,String)>=tx.query_row("SELECT thread_id,desired_model,state FROM operations WHERE interaction_id=?1 AND kind='direct_model'",[&interaction],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if let Some((saved_thread,saved_model,state))=previous {
                ensure!(saved_thread==thread && saved_model==model,"model selection interaction changed");
                return Ok(state=="APPLIED");
            }
            let latest:i64=tx.query_row("SELECT latest_model_sequence FROM conversations WHERE thread_id=?1",[&thread],|row|row.get(0))?;
            let applied=order>latest;
            if applied {
                tx.execute("UPDATE conversations SET selected_model=?2,selection_revision=selection_revision+1,latest_model_sequence=?3 WHERE thread_id=?1",params![thread,model,order])?;
            }
            tx.execute("INSERT INTO operations(id,interaction_id,thread_id,kind,desired_model,state,created_at) VALUES(?1,?2,?3,'direct_model',?4,?5,?6)",params![domain::id(),interaction,thread,model,if applied{"APPLIED"}else{"SUPERSEDED"},domain::now_ms()])?;
            tx.commit()?;
            Ok(applied)
        }).await
    }
}
