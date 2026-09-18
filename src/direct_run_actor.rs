//! Owns one live App Server child and serializes controls with its events.
//! The actor does not know Discord button layouts or log approval arguments.
use crate::{
    codex_execution::CodexExecution,
    codex_transport::Event,
    direct_application::{DirectApplication, DirectControlOutcome, DirectDeliveryResult},
    direct_approval::{DirectInteraction, ManualDecision},
    direct_run::ActiveRun,
    domain::RequestState,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

pub enum RunCommand {
    Approval {
        interaction_id: String,
        fingerprint: String,
        decision: ManualDecision,
        reply: oneshot::Sender<Result<()>>,
    },
    McpToolApproval {
        interaction_id: String,
        operation: DirectInteraction,
        decision: ManualDecision,
        reply: oneshot::Sender<Result<()>>,
    },
    Cancel {
        interaction_id: String,
        discord_thread_id: String,
        reply: oneshot::Sender<Result<DirectControlOutcome>>,
    },
    Stop {
        interaction_id: String,
        discord_thread_id: String,
        reply: oneshot::Sender<Result<DirectControlOutcome>>,
    },
    Steer {
        interaction_id: String,
        discord_thread_id: String,
        input: Vec<Value>,
        reply: oneshot::Sender<Result<()>>,
    },
}

pub enum RunEvent {
    Approval {
        interaction_id: String,
        operation: Box<DirectInteraction>,
    },
    UnsupportedApproval,
    Terminal(DirectDeliveryResult),
    DeliveryPending,
    ResultUnknown,
}

pub struct RunActor {
    pub commands: mpsc::Sender<RunCommand>,
    pub events: mpsc::Receiver<RunEvent>,
    pub task: JoinHandle<Result<()>>,
}

pub fn spawn(app: DirectApplication, run: ActiveRun) -> RunActor {
    let (commands, control_rx) = mpsc::channel(32);
    let (events, event_rx) = mpsc::channel(32);
    let task = tokio::spawn(serve(app, run, control_rx, events));
    RunActor {
        commands,
        events: event_rx,
        task,
    }
}

async fn serve(
    app: DirectApplication,
    mut run: ActiveRun,
    mut commands: mpsc::Receiver<RunCommand>,
    events: mpsc::Sender<RunEvent>,
) -> Result<()> {
    let mut seen_approvals = HashSet::new();
    let mut approval_rpc_ids = HashMap::<String, String>::new();
    let mut mcp_items = HashMap::<String, Option<Value>>::new();
    let mut terminal_seen = None::<Instant>;
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            command=commands.recv(), if !commands.is_closed()=>{
                if let Some(command)=command {
                    match command {
                        RunCommand::Approval{interaction_id,fingerprint,decision,reply}=>{
                            let result=app.runs.reply_manual(&run,interaction_id,fingerprint,decision).await;
                            let _=reply.send(result);
                        }
                        RunCommand::McpToolApproval{interaction_id,operation,decision,reply}=>{
                            let result=app.runs.reply_mcp_tool(&run,interaction_id,operation,decision).await;
                            let _=reply.send(result);
                        }
                        RunCommand::Cancel{interaction_id,discord_thread_id,reply}=>{
                            let result=app.cancel(&interaction_id,&discord_thread_id,Some(&run)).await;
                            let _=reply.send(result);
                        }
                        RunCommand::Stop{interaction_id,discord_thread_id,reply}=>{
                            let result=app.stop(&interaction_id,&discord_thread_id,Some(&run)).await;
                            let _=reply.send(result);
                        }
                        RunCommand::Steer{interaction_id,discord_thread_id,input,reply}=>{
                            let result=app.steer(&interaction_id,&discord_thread_id,&run,input).await;
                            let _=reply.send(result);
                        }
                    }
                }
            }
            event=run.recv()=>{
                match event {
                    Ok(event @ Event::ServerRequest{..})=>{
                        let evidence=match &event {
                            Event::ServerRequest{params,..}=>params["itemId"].as_str().and_then(|id|mcp_items.get(id)).and_then(Option::as_ref).cloned(),
                            _=>None,
                        };
                        match app.runs.register_interaction_with_evidence(&run,&event,evidence).await? {
                            Some((id,operation))=>{
                                approval_rpc_ids.insert(serde_json::to_string(&operation.rpc_id)?,id.clone());
                                if seen_approvals.insert(id.clone()) {
                                    emit(&events,RunEvent::Approval{interaction_id:id,operation:Box::new(operation)})?;
                                }
                            }
                            None=>{emit(&events,RunEvent::UnsupportedApproval)?;}
                        }
                    }
                    Ok(Event::Notification{method,params}) if method=="item/started"
                        && params["threadId"]==run.identity.thread_id
                        && params["turnId"]==run.identity.turn_id
                        && params["item"]["type"]=="mcpToolCall"=>{
                        if let Some(id)=params["item"]["id"].as_str() {
                            let bounded=serde_json::to_vec(&params["item"])?.len()<=crate::direct_approval::MAX_REQUEST_BYTES;
                            if !bounded {
                                if let Some(slot)=mcp_items.get_mut(id){*slot=None;}
                                continue;
                            }
                            if mcp_items.len()>=128 && !mcp_items.contains_key(id){continue;}
                            let item=params["item"].clone();
                            match mcp_items.entry(id.into()) {
                                std::collections::hash_map::Entry::Vacant(slot)=>{slot.insert(Some(item));}
                                std::collections::hash_map::Entry::Occupied(mut slot)=>{
                                    if slot.get().as_ref()!=Some(&item){slot.insert(None);}
                                }
                            }
                        }
                    }
                    Ok(Event::Notification{method,params}) if method=="serverRequest/resolved"=>{
                        if params["threadId"]==run.identity.thread_id
                            && let Some(request)=params.get("requestId")
                            && let Some(id)=approval_rpc_ids.remove(&serde_json::to_string(request)?) {
                            app.store.resolve_direct_approval(id).await?;
                        }
                    }
                    Ok(Event::Closed(_))|Err(tokio::sync::broadcast::error::RecvError::Closed)=>{
                        app.store.mark_direct_unknown(run.request_id.clone(),"runtime_closed".into()).await?;
                        emit(&events,RunEvent::ResultUnknown)?;
                        return Ok(());
                    }
                    Ok(Event::ProtocolError(_))|Ok(Event::InvalidServerRequest{..})=>{
                        app.store.mark_direct_unknown(run.request_id.clone(),"runtime_protocol_error".into()).await?;
                        emit(&events,RunEvent::ResultUnknown)?;
                        return Ok(());
                    }
                    Ok(Event::Notification{method,params}) if method=="turn/completed"=>{
                        ensure!(params["threadId"]==run.identity.thread_id && params["turn"]["id"]==run.identity.turn_id,"another turn completed on this child");
                        terminal_seen.get_or_insert_with(Instant::now);
                        if let Some(outcome)=finish_if_ready(&app,&run).await? {
                            emit(&events,outcome)?;
                            return Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{
                        // The polling branch below re-reads the exact Turn. Never infer completion.
                    }
                    _=>{}
                }
            }
            _=ticker.tick()=>{
                let execution=CodexExecution::new(run.transport().clone());
                if let Ok(Ok(snapshot))=tokio::time::timeout(Duration::from_secs(2),execution.read_turn(&run.identity)).await {
                    if matches!(snapshot.status.as_str(),"completed"|"failed"|"interrupted") {
                        terminal_seen.get_or_insert_with(Instant::now);
                        if let Some(outcome)=finish_if_ready(&app,&run).await? {
                            emit(&events,outcome)?;
                            return Ok(());
                        }
                    }
                } else if run.transport().is_closed() {
                    app.store.mark_direct_unknown(run.request_id.clone(),"runtime_closed".into()).await?;
                    emit(&events,RunEvent::ResultUnknown)?;
                    return Ok(());
                }
                if terminal_seen.is_some_and(|seen|seen.elapsed()>Duration::from_secs(60)) {
                    app.store.mark_direct_unknown(run.request_id.clone(),"terminal_content_unavailable".into()).await?;
                    emit(&events,RunEvent::ResultUnknown)?;
                    return Ok(());
                }
            }
        }
    }
}

fn emit(events: &mpsc::Sender<RunEvent>, event: RunEvent) -> Result<()> {
    events
        .try_send(event)
        .map_err(|_| anyhow::anyhow!("run event receiver unavailable or full"))
}

async fn finish_if_ready(app: &DirectApplication, run: &ActiveRun) -> Result<Option<RunEvent>> {
    match app.finish_and_deliver(run).await {
        Ok(result) => Ok(Some(RunEvent::Terminal(result))),
        Err(_) => {
            let state = app.store.request(&run.request_id).await?.state;
            if matches!(
                state,
                RequestState::Completed | RequestState::Failed | RequestState::Cancelled
            ) {
                Ok(Some(RunEvent::DeliveryPending))
            } else {
                Ok(None)
            }
        }
    }
}
