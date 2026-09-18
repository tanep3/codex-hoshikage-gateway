//! Owns one live App Server child and serializes controls with its events.
//! The actor does not know Discord button layouts or log approval arguments.
use crate::{
    codex_execution::CodexExecution,
    codex_transport::Event,
    direct_application::{DirectApplication, DirectControlOutcome, DirectDeliveryResult},
    direct_approval::{DirectInteraction, ManualDecision, RunGrantAudit},
    direct_run::ActiveRun,
    domain::RequestState,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
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
        input_generation: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    McpToolApproval {
        interaction_id: String,
        operation: DirectInteraction,
        decision: ManualDecision,
        run_grant: bool,
        input_generation: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    RejectUnsupported {
        interaction_id: String,
        fingerprint: String,
        input_generation: u64,
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
        input_generation: u64,
    },
    ApprovalResolved {
        interaction_id: String,
    },
    ApprovalInvalidated {
        input_generation: u64,
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

struct RunGrant {
    initial_interaction_id: String,
    config_fingerprint: String,
}

fn codex_config_fingerprint(home: &Path) -> Result<String> {
    let bytes = match std::fs::read(home.join("config.toml")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok("absent".into()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Codex config exceeds grant check limit"
    );
    Ok(crate::domain::digest(&bytes))
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
    let mut run_grants = HashMap::<(String, String), RunGrant>::new();
    let mut input_generation = 0_u64;
    let mut terminal_seen = None::<Instant>;
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            command=commands.recv(), if !commands.is_closed()=>{
                if let Some(command)=command {
                    match command {
                        RunCommand::Approval{interaction_id,fingerprint,decision,input_generation:expected,reply}=>{
                            let result=if expected==input_generation {app.runs.reply_manual(&run,interaction_id,fingerprint,decision).await}
                                else {Err(anyhow::anyhow!("approval belongs to an earlier input generation"))};
                            let _=reply.send(result);
                        }
                        RunCommand::McpToolApproval{interaction_id,operation,decision,run_grant,input_generation:expected,reply}=>{
                            let key=if run_grant {operation.run_grant_tool()} else {None};
                            let config=if run_grant {codex_config_fingerprint(&app.cfg.codex.home)} else {Ok(String::new())};
                            let result=if expected!=input_generation {
                                Err(anyhow::anyhow!("approval belongs to an earlier input generation"))
                            } else if run_grant && (decision!=ManualDecision::AcceptOnce || key.is_none()) {
                                Err(anyhow::anyhow!("Run grant is not eligible"))
                            } else if let Err(error)=&config {
                                Err(anyhow::anyhow!("Codex config cannot be checked: {error}"))
                            } else {
                                let audit=key.as_ref().map(|(server,tool)|RunGrantAudit::Selected{server:server.clone(),tool:tool.clone()});
                                app.runs.reply_mcp_tool(&run,interaction_id.clone(),operation,decision,audit).await
                            };
                            if result.is_ok() && let Some(key)=key {run_grants.insert(key,RunGrant{initial_interaction_id:interaction_id,config_fingerprint:config.unwrap()});}
                            let _=reply.send(result);
                        }
                        RunCommand::RejectUnsupported{interaction_id,fingerprint,input_generation:expected,reply}=>{
                            let result=if expected==input_generation {app.runs.reject_unsupported(&run,interaction_id,fingerprint).await}
                                else {Err(anyhow::anyhow!("approval belongs to an earlier input generation"))};
                            let _=reply.send(result);
                        }
                        RunCommand::Cancel{interaction_id,discord_thread_id,reply}=>{
                            run_grants.clear();
                            let result=app.cancel(&interaction_id,&discord_thread_id,Some(&run)).await;
                            let _=reply.send(result);
                        }
                        RunCommand::Stop{interaction_id,discord_thread_id,reply}=>{
                            run_grants.clear();
                            let result=app.stop(&interaction_id,&discord_thread_id,Some(&run)).await;
                            let _=reply.send(result);
                        }
                        RunCommand::Steer{interaction_id,discord_thread_id,input,reply}=>{
                            run_grants.clear();
                            input_generation=input_generation.saturating_add(1);
                            emit(&events,RunEvent::ApprovalInvalidated{input_generation})?;
                            let result=app.steer(&interaction_id,&discord_thread_id,&run,input).await;
                            let _=reply.send(result);
                        }
                    }
                }
            }
            event=run.recv()=>{
                match event {
                    Ok(Event::ServerRequest{id,method,params})
                        if method=="item/tool/call" && params["tool"]=="hoshikage_publish_artifact"=>{
                        ensure!(
                            params["threadId"]==run.identity.thread_id
                                && params["turnId"]==run.identity.turn_id,
                            "artifact tool belongs to another turn"
                        );
                        let outcome=async {
                            let call=params["callId"].as_str().ok_or_else(||anyhow::anyhow!("artifact call ID missing"))?;
                            let path=params["arguments"]["path"].as_str().ok_or_else(||anyhow::anyhow!("artifact path missing"))?;
                            let display=params["arguments"]["display_name"].as_str();
                            let thread=app.store.request(&run.request_id).await?.thread_id;
                            crate::direct_artifacts::capture(
                                &app.store,&app.runs.content,
                                crate::direct_artifacts::CaptureTarget {
                                    thread_id:&thread,request_id:Some(&run.request_id),
                                    call_id:call,relative:path,display_name:display
                                },
                                app.cfg.limits.artifact_bytes
                            ).await
                        }.await;
                        let response=match outcome {
                            Ok(artifact)=>serde_json::json!({"success":true,"contentItems":[{"type":"inputText","text":serde_json::json!({"artifact_id":artifact.id,"state":"ready"}).to_string()}]}),
                            Err(_)=>serde_json::json!({"success":false,"contentItems":[{"type":"inputText","text":"artifact_registration_failed"}]}),
                        };
                        run.transport().respond(id,response).await?;
                    }
                    Ok(event @ Event::ServerRequest{..})=>{
                        let rpc_id=match &event { Event::ServerRequest{id,..}=>id.clone(),_=>unreachable!() };
                        let matched_item=match &event {
                            Event::ServerRequest{method,..} if method=="mcpServer/elicitation/request"=>{
                                if mcp_items.len()!=1 {None} else {
                                    mcp_items.iter().find_map(|(id,item)|item.as_ref().filter(|item|
                                        DirectInteraction::from_event(&event,&run.identity)
                                            .ok().flatten().is_some_and(|mut interaction|
                                                interaction.bind_mcp_evidence((*item).clone()).is_ok())
                                    ).map(|item|(id.clone(),item.clone())))
                                }
                            }
                            Event::ServerRequest{params,..}=>params["itemId"].as_str().and_then(|id|
                                mcp_items.get(id).and_then(Option::as_ref).map(|item|(id.to_owned(),item.clone()))),
                            _=>None,
                        };
                        let evidence=matched_item.as_ref().map(|(_,item)|item.clone());
                        match app.runs.register_interaction_with_evidence(&run,&event,evidence).await? {
                            Some((id,operation))=>{
                                if let Some((item_id,_))=matched_item {mcp_items.remove(&item_id);}
                                approval_rpc_ids.insert(serde_json::to_string(&operation.rpc_id)?,id.clone());
                                let key=operation.run_grant_tool();
                                let current_config=codex_config_fingerprint(&app.cfg.codex.home).ok();
                                if current_config.as_ref().is_none_or(|current|run_grants.values().any(|grant|&grant.config_fingerprint!=current)) {
                                    run_grants.clear();
                                }
                                if let Some((server,tool))=key.as_ref()
                                    && let Some(grant)=run_grants.get(&(server.clone(),tool.clone())) {
                                    let audit=RunGrantAudit::Applied{initial_interaction_id:grant.initial_interaction_id.clone(),server:server.clone(),tool:tool.clone()};
                                    if app.runs.reply_mcp_tool(&run,id.clone(),operation,ManualDecision::AcceptOnce,Some(audit)).await.is_err() {
                                        run_grants.clear();
                                        app.store.mark_direct_unknown(run.request_id.clone(),"run_grant_reply_unknown".into()).await?;
                                        emit(&events,RunEvent::ResultUnknown)?;
                                        return Ok(());
                                    }
                                } else if seen_approvals.insert(id.clone()) {
                                    emit(&events,RunEvent::Approval{interaction_id:id,operation:Box::new(operation),input_generation})?;
                                }
                            }
                            None=>{
                                if let Err(error)=run.transport().reject(rpc_id,-32601,"unsupported App Server request").await {
                                    app.store.mark_direct_unknown(run.request_id.clone(),"unsupported_request_reply_unknown".into()).await?;
                                    emit(&events,RunEvent::ResultUnknown)?;
                                    return Err(error.into());
                                }
                                emit(&events,RunEvent::UnsupportedApproval)?;
                            }
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
                    Ok(Event::Notification{method,params}) if method=="item/completed"
                        && params["threadId"]==run.identity.thread_id
                        && params["turnId"]==run.identity.turn_id
                        && params["item"]["type"]=="mcpToolCall"=>{
                        if let Some(id)=params["item"]["id"].as_str(){mcp_items.remove(id);}
                    }
                    Ok(Event::Notification{method,params}) if method=="serverRequest/resolved"=>{
                        if params["threadId"]==run.identity.thread_id
                            && let Some(request)=params.get("requestId")
                            && let Some(id)=approval_rpc_ids.remove(&serde_json::to_string(request)?) {
                            app.store.resolve_direct_approval(id.clone()).await?;
                            emit(&events,RunEvent::ApprovalResolved{interaction_id:id})?;
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
