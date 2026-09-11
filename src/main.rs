use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use codex_hoshikage_gateway::{
    admin,
    application::App,
    backup,
    config::{Config, secret},
    discord::{Discord, Handler},
    domain,
    proxy::Proxy,
    storage,
};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};
#[derive(Parser)]
#[command(
    name = "codex-hoshikage-gateway",
    version,
    about = "Codex Hoshikage Gateway"
)]
struct Cli {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Check,
    Init,
    Run {
        #[arg(long)]
        recovery: bool,
    },
    Restore {
        #[arg(long)]
        from: PathBuf,
    },
    Admin {
        #[command(subcommand)]
        command: Admin,
    },
}
#[derive(Subcommand)]
enum Admin {
    Status,
    ProxyInspect,
    ProxyAccept {
        #[arg(long)]
        review_token: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        accept_risk: bool,
    },
    Reload,
    Reconcile {
        #[arg(long)]
        request_id: String,
    },
    Abandon {
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        accept_risk: bool,
    },
    Backup {
        #[arg(long)]
        to: PathBuf,
    },
    Recovery {
        #[command(subcommand)]
        command: Recovery,
    },
}
#[derive(Subcommand)]
enum Recovery {
    Release {
        #[arg(long)]
        restore_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        accept_risk: bool,
    },
}
#[tokio::main]
async fn main() {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("Gateway内部タスクの異常を検出しました（内容は非表示）。")
    }));
    tracing_subscriber::fmt()
        .with_env_filter("codex_hoshikage_gateway=info")
        .with_target(false)
        .init();
    if let Err(error) = run(Cli::parse()).await {
        // No arbitrary Debug/Display chain: dependencies can embed tokens, paths, or request bodies.
        eprintln!(
            "Gateway処理に失敗しました。分類: {}",
            error
                .to_string()
                .split(':')
                .next()
                .unwrap_or("不明")
                .chars()
                .take(120)
                .collect::<String>()
        );
        std::process::exit(1);
    }
}
async fn run(cli: Cli) -> Result<()> {
    let config_path = cli
        .config
        .canonicalize()
        .context("設定ファイルを読み込めません")?;
    let cfg = Config::read(&config_path)?;
    match cli.command {
        Command::Check => {
            let cfg = cfg
                .clone()
                .with_registered_projects(&storage::db_path(&cfg))?;
            cfg.validate()?;
            secret(&cfg.discord.token_file)?;
            secret(&cfg.proxy.api_key_file)?;
            println!("設定・認証ファイルのローカル検証に成功しました。外部接続は未検証です。");
        }
        Command::Init => {
            ensure!(
                !admin::binding(&config_path)?.exists(),
                "インスタンスは初期化済みです"
            );
            let instance = storage::initialize(&cfg)?;
            backup::atomic_new(
                &admin::binding(&config_path)?,
                &serde_json::to_vec(
                    &serde_json::json!({"instance_uuid":instance,"state_dir":cfg.storage.state_dir}),
                )?,
            )?;
            println!("状態DBを初期化しました。instance_uuid={instance}");
        }
        Command::Restore { from } => {
            admin::validate_binding(&cfg, &config_path)?;
            let id = backup::restore(&cfg, &config_path, &from)?;
            println!("復元しました。復元保留は継続中です。restore_id={id}");
        }
        Command::Admin { command } => {
            let cmd = match command {
                Admin::ProxyInspect => admin::Command::ProxyInspect,
                Admin::ProxyAccept {
                    review_token,
                    reason,
                    accept_risk,
                } => admin::Command::ProxyAccept {
                    review_token,
                    reason,
                    accept_risk,
                },
                Admin::Status => admin::Command::Status,
                Admin::Reload => admin::Command::Reload,
                Admin::Reconcile { request_id } => admin::Command::Reconcile { request_id },
                Admin::Abandon {
                    request_id,
                    generation,
                    reason,
                    accept_risk,
                } => admin::Command::Abandon {
                    request_id,
                    generation,
                    reason,
                    accept_risk,
                },
                Admin::Backup { to } => admin::Command::Backup { to },
                Admin::Recovery {
                    command:
                        Recovery::Release {
                            restore_id,
                            reason,
                            accept_risk,
                        },
                } => admin::Command::RecoveryRelease {
                    restore_id,
                    reason,
                    accept_risk,
                },
            };
            let result = admin::call(&cfg.storage.socket_path, cmd).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            ensure!(result["ok"] == true, "管理操作に失敗しました");
        }
        Command::Run { recovery } => daemon(cfg, config_path, recovery).await?,
    }
    Ok(())
}
async fn daemon(cfg: Config, config_path: PathBuf, force_recovery: bool) -> Result<()> {
    let cfg = cfg
        .clone()
        .with_registered_projects(&storage::db_path(&cfg))?;
    cfg.validate()?;
    let _lock = storage::StateLock::acquire(&cfg.storage.state_dir)?;
    admin::validate_binding(&cfg, &config_path)?;
    let marker = backup::marker(&config_path)?;
    let (store, done) = if marker.exists() || force_recovery {
        storage::Store::open_recovery(&cfg)?
    } else {
        storage::Store::open(&cfg)?
    };
    if force_recovery && !marker.exists() {
        let (_, instance) = storage::validate_database(&store.path)?;
        backup::atomic_new(
            &marker,
            &serde_json::to_vec(
                &serde_json::json!({"restore_id":domain::id(),"instance_uuid":instance,"state_dir":cfg.storage.state_dir}),
            )?,
        )?;
    }
    let pending = if marker.exists() {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
        let (_, instance) = storage::validate_database(&store.path)?;
        ensure!(
            v["instance_uuid"] == instance
                && v["state_dir"] == cfg.storage.state_dir.to_string_lossy().as_ref(),
            "復元マーカーが一致しません"
        );
        backup::register_recovery_projects(&store, &cfg).await?;
        backup::quarantine(
            &store,
            v["restore_id"]
                .as_str()
                .context("復元IDがありません")?
                .into(),
        )
        .await?;
        true
    } else {
        store.status().await?["recovery_pending"] == true
    };
    store.startup_recover().await?;
    let token = secret(&cfg.discord.token_file)?;
    let proxy = Proxy::new(cfg.proxy.base_url.clone(), secret(&cfg.proxy.api_key_file)?)?;
    let discord = Discord::new(token.clone())?;
    codex_hoshikage_gateway::resources::clean_orphan_cache(&cfg.storage.temp_dir)?;
    let app = App::new(cfg.clone(), store, discord, proxy)?;
    app.recovery.store(pending, Ordering::SeqCst);
    app.settings.write().await.revision = app
        .store
        .call(true, |c| {
            Ok(c.query_row("SELECT config_revision FROM schema_meta", [], |r| r.get(0))?)
        })
        .await?;
    let (messages, messages_rx) = mpsc::channel(128);
    let (controls, controls_rx) = mpsc::channel(64);
    let intents = serenity::all::GatewayIntents::GUILDS
        | serenity::all::GatewayIntents::GUILD_MESSAGES
        | serenity::all::GatewayIntents::MESSAGE_CONTENT;
    let shard_slot: std::sync::Arc<
        tokio::sync::Mutex<Option<std::sync::Arc<serenity::gateway::ShardManager>>>,
    > = Default::default();
    let mut jobs = JoinSet::new();
    macro_rules! task {
        ($name:literal,$future:expr) => {
            jobs.spawn(async move { ($name, $future.await) });
        };
    }
    let a = app.clone();
    task!("admission", a.admit_loop(messages_rx));
    let a = app.clone();
    task!("control", a.control_loop(controls_rx));
    let a = app.clone();
    task!("scheduler", a.scheduler_loop());
    let a = app.clone();
    task!("capability", a.capability_loop());
    let a = app.clone();
    task!("sweeper", a.sweep_loop());
    let a = app.clone();
    task!("monitor", a.monitor_loop());
    let a = app.clone();
    task!("events", a.event_loop());
    let a = app.clone();
    task!("delivery", a.delivery_loop());
    let a = app.clone();
    task!("resources", a.resource_loop());
    let a = app.clone();
    task!("retention", a.retention_loop());
    let a = app.clone();
    task!("admin", admin::serve(a, config_path));
    let a = app.clone();
    let slot = shard_slot.clone();
    task!("discord", async move {
        while a.recovery.load(Ordering::SeqCst) {
            tokio::select! {_=a.cancel.cancelled()=>return Ok(()),_=tokio::time::sleep(Duration::from_millis(500))=>{}}
        }
        let mut client = serenity::Client::builder(token, intents)
            .event_handler(codex_hoshikage_gateway::discord::Health {
                connected: a.connected.clone(),
            })
            .raw_event_handler(Handler {
                messages,
                controls,
                cancel: a.cancel.clone(),
            })
            .await?;
        *slot.lock().await = Some(client.shard_manager.clone());
        client.start().await.map_err(anyhow::Error::from)
    });
    task!("store", async {
        let _ = done.await;
        Err::<(), _>(anyhow::anyhow!("Store終了"))
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let failed = tokio::select! {
        _=tokio::signal::ctrl_c()=>false,
        _=term.recv()=>false,
        _=app.cancel.cancelled()=>true,
        result=jobs.join_next()=>{match result{Some(Ok((role,_)))=>tracing::error!(role,event="critical_task_exited"),_=>tracing::error!(event="critical_task_panicked")};true},
    };
    app.connected.store(false, Ordering::SeqCst);
    app.settings().await.proxy.gate.invalidate();
    app.cancel.cancel();
    if let Some(shards) = shard_slot.lock().await.clone() {
        let _ = tokio::time::timeout(Duration::from_secs(10), shards.shutdown_all()).await;
    }
    // Store worker is ended by process teardown after clients drop; all Tokio workers are joined/aborted.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while jobs.join_next().await.is_some() {}
    })
    .await;
    jobs.shutdown().await;
    ensure!(!failed, "必須タスク異常により受付を停止しました");
    Ok(())
}
