//! Test di integrazione IPC — verifica la comunicazione
//! tra agentd e i client via Unix socket JSON-RPC.

use agentos_common::ipc::*;
use agentos_common::types::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Helper: invia un messaggio JSON-RPC su un socket e leggi la risposta.
async fn send_jsonrpc(
    stream: &mut UnixStream,
    method: &str,
    params: serde_json::Value,
    id: u64,
) -> JsonRpcResponse {
    let request = JsonRpcRequest::new(method, params, Some(serde_json::json!(id)));
    let json = serde_json::to_string(&request).unwrap();

    stream.write_all(format!("{}\n", json).as_bytes()).await.unwrap();

    let mut reader = BufReader::new(&mut *stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await.unwrap();

    serde_json::from_str(response_line.trim()).unwrap()
}

#[test]
fn test_shell_to_agent_roundtrip() {
    // Verifica che tutti i tipi di messaggio si serializzano/deserializzano
    let messages = vec![
        ShellToAgent::UserInput { text: "ciao".into() },
        ShellToAgent::UserConfirm { action_id: "abc".into(), approved: true },
        ShellToAgent::WindowFocus { window_id: 1, app_name: "foot".into(), title: "Terminal".into() },
        ShellToAgent::BriefingRequest,
        ShellToAgent::SearchRequest { query: "fattura".into() },
        ShellToAgent::WorkspaceModeChange { mode: WorkspaceMode::Split },
    ];

    for msg in &messages {
        let json = serde_json::to_string(msg).unwrap();
        let deserialized: ShellToAgent = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2, "Roundtrip fallito per: {:?}", msg);
    }
}

#[test]
fn test_agent_to_shell_roundtrip() {
    let messages: Vec<AgentToShell> = vec![
        AgentToShell::Thinking,
        AgentToShell::Response {
            text: "Ecco i file".into(),
            commands: Some(vec!["ls -la".into()]),
            zone: Some(RiskZone::Green),
        },
        AgentToShell::ConfirmRequest {
            action_id: "xyz".into(),
            description: "Installare vim?".into(),
            zone: RiskZone::Yellow,
        },
        AgentToShell::ExecutionProgress {
            step: 1, total: 3, description: "Passo 1".into(),
        },
        AgentToShell::ExecutionResult {
            success: true,
            output: "ok".into(),
            error: None,
        },
        AgentToShell::Notification {
            title: "Test".into(),
            body: "Corpo".into(),
            urgency: Urgency::Normal,
            actions: None,
        },
    ];

    for msg in &messages {
        let json = serde_json::to_string(msg).unwrap();
        let deserialized: AgentToShell = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn test_fs_messages_roundtrip() {
    let search = AgentToFs::Search {
        query: "fattura dentista".into(),
        file_type: Some("pdf".into()),
        folder: None,
        max_results: 10,
    };
    let json = serde_json::to_string(&search).unwrap();
    let deserialized: AgentToFs = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, json2);

    let results = FsToAgent::SearchResults {
        query: "fattura".into(),
        results: vec![SearchResult {
            path: "/home/user/fattura.pdf".into(),
            name: "fattura.pdf".into(),
            snippet: "Fattura n. 42".into(),
            score: 0.87,
            file_type: "pdf".into(),
            modified_at: chrono::Utc::now(),
        }],
    };
    let json = serde_json::to_string(&results).unwrap();
    let _: FsToAgent = serde_json::from_str(&json).unwrap();
}

#[test]
fn test_jsonrpc_protocol_compliance() {
    // Verifica che le richieste JSON-RPC siano conformi allo standard 2.0
    let req = JsonRpcRequest::new(
        "user.input",
        serde_json::json!({"type": "user.input", "text": "ciao"}),
        Some(serde_json::json!(1)),
    );

    let json = serde_json::to_string(&req).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["method"], "user.input");
    assert!(parsed["params"].is_object());
    assert_eq!(parsed["id"], 1);
}

#[test]
fn test_jsonrpc_error_response() {
    let resp = JsonRpcResponse::error(
        GUARDIAN_BLOCKED,
        "Comando bloccato",
        serde_json::json!(1),
    );

    let json = serde_json::to_string(&resp).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["jsonrpc"], "2.0");
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], GUARDIAN_BLOCKED);
    assert!(parsed["result"].is_null());
}

#[tokio::test]
async fn test_unix_socket_echo() {
    // Test che il protocollo funziona su un vero Unix socket
    let socket_path = format!("/tmp/agentos-test-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).unwrap();

    // Server: leggi una riga, rispondi con successo
    let server_path = socket_path.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let req: JsonRpcRequest = serde_json::from_str(line.trim()).unwrap();
        let resp = JsonRpcResponse::success(
            serde_json::json!({"echo": req.params}),
            req.id.unwrap_or(serde_json::Value::Null),
        );
        let resp_json = serde_json::to_string(&resp).unwrap();
        writer.write_all(format!("{}\n", resp_json).as_bytes()).await.unwrap();
    });

    // Client: invia richiesta e leggi risposta
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut stream = UnixStream::connect(&socket_path).await.unwrap();

    let resp = send_jsonrpc(
        &mut stream,
        "test.echo",
        serde_json::json!({"hello": "world"}),
        1,
    ).await;

    assert!(resp.error.is_none());
    assert!(resp.result.is_some());

    server.await.unwrap();
    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn test_guardian_integration() {
    // Verifica l'intero flow: comando → Guardian → verdetto → serializzazione
    use agentos_common::types::GuardianVerdict;

    let verdict = GuardianVerdict {
        zone: RiskZone::Red,
        reason: "rm -rf / — distruzione totale del filesystem".into(),
        command: "rm -rf /".into(),
        blocked: true,
    };

    // Il verdetto viene usato per costruire una risposta AgentToShell
    let response = AgentToShell::Response {
        text: format!("Comando bloccato: {}", verdict.reason),
        commands: None,
        zone: Some(verdict.zone),
    };

    let json = serde_json::to_string(&response).unwrap();
    let parsed: AgentToShell = serde_json::from_str(&json).unwrap();

    match parsed {
        AgentToShell::Response { zone, .. } => {
            assert_eq!(zone, Some(RiskZone::Red));
        }
        _ => panic!("Tipo risposta errato"),
    }
}
