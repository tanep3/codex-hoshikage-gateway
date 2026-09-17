//! Opt-in real App Server test, run immediately after transport acceptance.
//! Uses a private CODEX_HOME and two private workspaces; never touches Discord.
use codex_hoshikage_gateway::{
    codex_execution::CodexExecution,
    codex_transport::{CodexTransport, Event, LaunchConfig},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};

#[tokio::test]
#[ignore = "uses real Codex model calls and local authentication"]
async fn one_child_runs_two_isolated_threads() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let left = root.path().join("left");
    let right = root.path().join("right");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    let auth = PathBuf::from(std::env::var("HOME").unwrap()).join(".codex/auth.json");
    fs::copy(auth, home.join("auth.json")).unwrap();
    let transport = CodexTransport::launch(&LaunchConfig {
        command: PathBuf::from("codex"),
        args: vec!["app-server".into(), "--listen".into(), "stdio://".into()],
        codex_home: home,
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(30),
        experimental_api: true,
    })
    .await
    .unwrap();
    let mut events = transport.subscribe();
    let models = CodexExecution::new(transport.clone())
        .list_models()
        .await
        .unwrap();
    assert!(models.iter().any(|model| model["id"] == "gpt-5.6-luna"));
    let start = |cwd: &PathBuf| {
        json!({
            "cwd": cwd, "modelProvider":"openai", "model":"gpt-5.6-luna",
            "approvalPolicy":"never", "sandbox":"workspace-write"
        })
    };
    let (a, b) = tokio::join!(
        transport.request("thread/start", start(&left)),
        transport.request("thread/start", start(&right)),
    );
    let a = a.unwrap()["thread"]["id"].as_str().unwrap().to_owned();
    let b = b.unwrap()["thread"]["id"].as_str().unwrap().to_owned();
    assert_ne!(a, b);
    let turn = |thread: &str, marker: &str| {
        json!({
            "threadId":thread,
            "input":[{"type":"text","text":format!("Reply with exactly {marker}. Do not call tools.")}],
            "approvalPolicy":"never"
        })
    };
    let (first, second) = tokio::join!(
        transport.request("turn/start", turn(&a, "LEFT_MARKER")),
        transport.request("turn/start", turn(&b, "RIGHT_MARKER")),
    );
    let first = first.unwrap()["turn"]["id"].as_str().unwrap().to_owned();
    let second = second.unwrap()["turn"]["id"].as_str().unwrap().to_owned();
    let mut finished = [false, false];
    tokio::time::timeout(Duration::from_secs(150), async {
        while !finished.iter().all(|x| *x) {
            match events.recv().await.unwrap() {
                Event::Notification { method, params } if method == "turn/completed" => {
                    let pair = (params["threadId"].as_str(), params["turn"]["id"].as_str());
                    if pair == (Some(&a), Some(&first)) {
                        finished[0] = true;
                    }
                    if pair == (Some(&b), Some(&second)) {
                        finished[1] = true;
                    }
                }
                Event::ServerRequest { id, .. } => {
                    transport
                        .reject(id, -32601, "No tools are authorized in this probe")
                        .await
                        .unwrap();
                }
                Event::Closed(reason) => {
                    panic!("App Server closed during parallel turns: {reason}")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let (read_a, read_b) = tokio::join!(
        transport.request("thread/read", json!({"threadId":a,"includeTurns":true})),
        transport.request("thread/read", json!({"threadId":b,"includeTurns":true})),
    );
    let text = |thread: Value| thread["thread"]["turns"].to_string();
    let left_output = text(read_a.unwrap());
    let right_output = text(read_b.unwrap());
    assert!(left_output.contains("LEFT_MARKER"));
    assert!(!left_output.contains("RIGHT_MARKER"));
    assert!(right_output.contains("RIGHT_MARKER"));
    assert!(!right_output.contains("LEFT_MARKER"));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "uses real Codex model calls and local authentication"]
async fn a_new_owned_child_resumes_the_previous_thread() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let work = root.path().join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    let auth = PathBuf::from(std::env::var("HOME").unwrap()).join(".codex/auth.json");
    fs::copy(auth, home.join("auth.json")).unwrap();
    let config = LaunchConfig {
        command: PathBuf::from("codex"),
        args: vec!["app-server".into(), "--listen".into(), "stdio://".into()],
        codex_home: home,
        initialize_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(30),
        experimental_api: true,
    };
    let first = CodexTransport::launch(&config).await.unwrap();
    let started = first
        .request(
            "thread/start",
            json!({
                "cwd":work,"modelProvider":"openai","model":"gpt-5.6-luna",
                "approvalPolicy":"never","sandbox":"workspace-write"
            }),
        )
        .await
        .unwrap();
    let thread = started["thread"]["id"].as_str().unwrap().to_owned();
    let mut first_events = first.subscribe();
    let first_turn = first.request("turn/start", json!({
        "threadId":thread,"input":[{"type":"text","text":"Reply with FIRST_OK. Do not call tools."}],
        "approvalPolicy":"never"
    })).await.unwrap();
    let first_turn_id = first_turn["turn"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            if let Event::Notification { method, params } = first_events.recv().await.unwrap()
                && method == "turn/completed"
                && params["threadId"] == thread
                && params["turn"]["id"] == first_turn_id
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    first.shutdown().await.unwrap();
    let second = CodexTransport::launch(&config).await.unwrap();
    let resumed = second
        .request(
            "thread/resume",
            json!({
                "threadId":thread,"cwd":work,"modelProvider":"openai","model":"gpt-5.6-luna"
            }),
        )
        .await
        .unwrap();
    assert_eq!(resumed["thread"]["id"], thread);
    let mut events = second.subscribe();
    let started_turn = second.request("turn/start",json!({
        "threadId":thread,"input":[{"type":"text","text":"Reply with RESUMED_OK. Do not call tools."}],
        "approvalPolicy":"never"
    })).await.unwrap();
    let turn = started_turn["turn"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            match events.recv().await.unwrap() {
                Event::Notification { method, params }
                    if method == "turn/completed"
                        && params["threadId"] == thread
                        && params["turn"]["id"] == turn =>
                {
                    break;
                }
                Event::ServerRequest { id, .. } => {
                    second
                        .reject(id, -32601, "No tools are authorized in this probe")
                        .await
                        .unwrap();
                }
                Event::Closed(reason) => panic!("child closed: {reason}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let read = second
        .request(
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await
        .unwrap();
    assert!(read["thread"]["turns"].to_string().contains("RESUMED_OK"));
    second.shutdown().await.unwrap();
}
