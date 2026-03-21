//! Protocollo IPC JSON-RPC 2.0 tra i componenti di AgentOS.
//!
//! La comunicazione avviene tramite Unix socket:
//! - agent-shell ↔ agentd: `/run/agentd.sock`
//! - agent-fs ↔ agentd: `/run/agentd-fs.sock`

use serde::{Deserialize, Serialize};
use crate::types::*;

// ============================================================
// Messaggi JSON-RPC 2.0
// ============================================================

/// Wrapper JSON-RPC 2.0 per richieste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
}

/// Wrapper JSON-RPC 2.0 per risposte.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: serde_json::Value,
}

/// Errore JSON-RPC 2.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// Codici errore JSON-RPC standard
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// Codici errore custom AgentOS
pub const GUARDIAN_BLOCKED: i32 = -32000;
pub const LLM_ERROR: i32 = -32001;
pub const EXECUTION_ERROR: i32 = -32002;
pub const TIMEOUT_ERROR: i32 = -32003;

// ============================================================
// Messaggi shell → agentd
// ============================================================

/// Messaggi inviati da agent-shell ad agentd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellToAgent {
    /// Input testuale dall'utente
    #[serde(rename = "user.input")]
    UserInput { text: String },

    /// Conferma/rifiuto di un'azione in zona gialla
    #[serde(rename = "user.confirm")]
    UserConfirm {
        action_id: String,
        approved: bool,
    },

    /// La finestra in focus è cambiata
    #[serde(rename = "window.focus")]
    WindowFocus {
        window_id: u64,
        app_name: String,
        title: String,
    },

    /// Richiesta del briefing
    #[serde(rename = "briefing.request")]
    BriefingRequest,

    /// Richiesta di ricerca file
    #[serde(rename = "search.request")]
    SearchRequest { query: String },

    /// Cambio modalità workspace
    #[serde(rename = "workspace.mode")]
    WorkspaceModeChange { mode: WorkspaceMode },
}

// ============================================================
// Messaggi agentd → shell
// ============================================================

/// Messaggi inviati da agentd ad agent-shell.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentToShell {
    /// L'agente sta elaborando (indicatore "pensando...")
    #[serde(rename = "agent.thinking")]
    Thinking,

    /// Risposta dell'agente
    #[serde(rename = "agent.response")]
    Response {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        zone: Option<RiskZone>,
    },

    /// Richiesta di conferma per azione in zona gialla
    #[serde(rename = "agent.confirm_request")]
    ConfirmRequest {
        action_id: String,
        description: String,
        zone: RiskZone,
    },

    /// Progresso dell'esecuzione
    #[serde(rename = "execution.progress")]
    ExecutionProgress {
        step: u32,
        total: u32,
        description: String,
    },

    /// Risultato dell'esecuzione
    #[serde(rename = "execution.result")]
    ExecutionResult {
        success: bool,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Aggiornamento del briefing
    #[serde(rename = "briefing.update")]
    BriefingUpdate {
        #[serde(skip_serializing_if = "Option::is_none")]
        emails: Option<Vec<EmailSummary>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        calendar: Option<Vec<CalendarEvent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        files: Option<Vec<RecentFile>>,
    },

    /// Notifica per l'utente
    #[serde(rename = "agent.notification")]
    Notification {
        title: String,
        body: String,
        urgency: Urgency,
        #[serde(skip_serializing_if = "Option::is_none")]
        actions: Option<Vec<NotifAction>>,
    },

    /// Richiesta di riorganizzazione finestre
    #[serde(rename = "workspace.arrange")]
    WorkspaceArrange {
        layout: Vec<WindowPlacement>,
    },

    /// Risultati di ricerca semantica
    #[serde(rename = "search.results")]
    SearchResults {
        query: String,
        results: Vec<SearchResult>,
    },
}

// ============================================================
// Messaggi agent-fs ↔ agentd
// ============================================================

/// Messaggi inviati da agent-fs ad agentd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FsToAgent {
    /// Risultati di una ricerca semantica
    #[serde(rename = "fs.search_results")]
    SearchResults {
        query: String,
        results: Vec<SearchResult>,
    },

    /// Stato dell'indicizzazione
    #[serde(rename = "fs.index_status")]
    IndexStatus {
        total_files: u64,
        indexed_files: u64,
        pending_files: u64,
    },
}

/// Messaggi inviati da agentd ad agent-fs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentToFs {
    /// Richiesta di ricerca semantica
    #[serde(rename = "fs.search")]
    Search {
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
        max_results: u32,
    },

    /// Richiesta di reindicizzazione
    #[serde(rename = "fs.reindex")]
    Reindex {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// Richiesta stato
    #[serde(rename = "fs.status")]
    StatusRequest,
}

// ============================================================
// Funzioni helper per costruire messaggi JSON-RPC
// ============================================================

impl JsonRpcRequest {
    /// Crea una nuova richiesta JSON-RPC 2.0.
    pub fn new(method: &str, params: serde_json::Value, id: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id,
        }
    }

    /// Crea una richiesta da un messaggio ShellToAgent.
    pub fn from_shell_message(msg: &ShellToAgent, id: u64) -> Result<Self, serde_json::Error> {
        let params = serde_json::to_value(msg)?;
        let method = match msg {
            ShellToAgent::UserInput { .. } => "user.input",
            ShellToAgent::UserConfirm { .. } => "user.confirm",
            ShellToAgent::WindowFocus { .. } => "window.focus",
            ShellToAgent::BriefingRequest => "briefing.request",
            ShellToAgent::SearchRequest { .. } => "search.request",
            ShellToAgent::WorkspaceModeChange { .. } => "workspace.mode",
        };
        Ok(Self::new(method, params, Some(serde_json::Value::Number(id.into()))))
    }
}

impl JsonRpcResponse {
    /// Crea una risposta di successo.
    pub fn success(result: serde_json::Value, id: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Crea una risposta di errore.
    pub fn error(code: i32, message: &str, id: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_request_creation() {
        let req = JsonRpcRequest::new(
            "user.input",
            serde_json::json!({"text": "mostrami i file"}),
            Some(serde_json::json!(1)),
        );
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "user.input");

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn test_json_rpc_response_success() {
        let resp = JsonRpcResponse::success(
            serde_json::json!({"status": "ok"}),
            serde_json::json!(1),
        );
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_json_rpc_response_error() {
        let resp = JsonRpcResponse::error(
            GUARDIAN_BLOCKED,
            "Comando bloccato dal Guardian",
            serde_json::json!(1),
        );
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, GUARDIAN_BLOCKED);
    }

    #[test]
    fn test_shell_to_agent_serialization() {
        let msg = ShellToAgent::UserInput {
            text: "mostrami i file".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"user.input\""));
        assert!(json.contains("mostrami i file"));

        let deserialized: ShellToAgent = serde_json::from_str(&json).unwrap();
        match deserialized {
            ShellToAgent::UserInput { text } => assert_eq!(text, "mostrami i file"),
            _ => panic!("Tipo messaggio errato"),
        }
    }

    #[test]
    fn test_agent_to_shell_response() {
        let msg = AgentToShell::Response {
            text: "Ecco i file nella directory corrente:".to_string(),
            commands: Some(vec!["ls -la".to_string()]),
            zone: Some(RiskZone::Green),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent.response\""));

        let deserialized: AgentToShell = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentToShell::Response { text, commands, zone } => {
                assert!(text.contains("file"));
                assert_eq!(commands.unwrap().len(), 1);
                assert_eq!(zone.unwrap(), RiskZone::Green);
            }
            _ => panic!("Tipo messaggio errato"),
        }
    }

    #[test]
    fn test_agent_to_shell_confirm_request() {
        let msg = AgentToShell::ConfirmRequest {
            action_id: "abc123".to_string(),
            description: "Installare il pacchetto vim".to_string(),
            zone: RiskZone::Yellow,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent.confirm_request\""));
        assert!(json.contains("abc123"));
    }

    #[test]
    fn test_from_shell_message() {
        let msg = ShellToAgent::UserInput {
            text: "ciao".to_string(),
        };
        let req = JsonRpcRequest::from_shell_message(&msg, 42).unwrap();
        assert_eq!(req.method, "user.input");
        assert_eq!(req.id, Some(serde_json::json!(42)));
    }

    #[test]
    fn test_fs_to_agent_serialization() {
        let msg = FsToAgent::IndexStatus {
            total_files: 1000,
            indexed_files: 750,
            pending_files: 250,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"fs.index_status\""));
        assert!(json.contains("1000"));
    }

    #[test]
    fn test_agent_to_fs_search() {
        let msg = AgentToFs::Search {
            query: "fattura dentista".to_string(),
            file_type: Some("pdf".to_string()),
            folder: None,
            max_results: 10,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("fattura dentista"));
        assert!(json.contains("\"max_results\":10"));
    }

    #[test]
    fn test_all_shell_to_agent_variants() {
        // Verifica che tutti i varianti si serializzano correttamente
        let messages: Vec<ShellToAgent> = vec![
            ShellToAgent::UserInput { text: "test".to_string() },
            ShellToAgent::UserConfirm { action_id: "id1".to_string(), approved: true },
            ShellToAgent::WindowFocus { window_id: 1, app_name: "foot".to_string(), title: "Terminal".to_string() },
            ShellToAgent::BriefingRequest,
            ShellToAgent::SearchRequest { query: "test".to_string() },
            ShellToAgent::WorkspaceModeChange { mode: WorkspaceMode::Split },
        ];

        for msg in &messages {
            let json = serde_json::to_string(msg).unwrap();
            let _: ShellToAgent = serde_json::from_str(&json).unwrap();
        }
    }
}
