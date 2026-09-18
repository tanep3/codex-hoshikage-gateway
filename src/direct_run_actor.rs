//! Owns one live App Server child and serializes controls with its events.
//! The actor does not know Discord button layouts or log approval arguments.
use crate::{
    codex_execution::CodexExecution,
    codex_transport::Event,
    direct_application::{DirectApplication, DirectControlOutcome, DirectDeliveryResult},
    direct_approval::{DirectInteraction, ManualDecision},
    direct_run::ActiveRun,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
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
        operation: DirectInteraction,
    },
    UnsupportedApproval,
    Terminal(DirectDeliveryResult),
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
                        match app.runs.register_interaction(&run,&event).await? {
                            Some((id,operation))=>{
                                if seen_approvals.insert(id.clone()) {
                                    events.send(RunEvent::Approval{interaction_id:id,operation}).await?;
                                }
                            }
                            None=>{events.send(RunEvent::UnsupportedApproval).await?;}
                        }
                    }
                    Ok(Event::Closed(_))|Err(tokio::sync::broadcast::error::RecvError::Closed)=>{
                        app.store.mark_direct_unknown(run.request_id.clone(),"runtime_closed".into()).await?;
                        events.send(RunEvent::ResultUnknown).await?;
                        return Ok(());
                    }
                    Ok(Event::ProtocolError(_))|Ok(Event::InvalidServerRequest{..})=>{
                        app.store.mark_direct_unknown(run.request_id.clone(),"runtime_protocol_error".into()).await?;
                        events.send(RunEvent::ResultUnknown).await?;
                        return Ok(());
                    }
                    Ok(Event::Notification{method,params}) if method=="turn/completed"=>{
                        ensure!(params["threadId"]==run.identity.thread_id && params["turn"]["id"]==run.identity.turn_id,"another turn completed on this child");
                        if let Ok(result)=app.finish_and_deliver(&run).await {
                            events.send(RunEvent::Terminal(result)).await?;
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
                    if matches!(snapshot.status.as_str(),"completed"|"failed"|"interrupted")
                        && let Ok(result)=app.finish_and_deliver(&run).await {
                        events.send(RunEvent::Terminal(result)).await?;
                        return Ok(());
                    }
                } else if run.transport().is_closed() {
                    app.store.mark_direct_unknown(run.request_id.clone(),"runtime_closed".into()).await?;
                    events.send(RunEvent::ResultUnknown).await?;
                    return Ok(());
                }
            }
        }
    }
}
