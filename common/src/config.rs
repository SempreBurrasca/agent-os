//! Configurazione di AgentOS.
//! Caricata da `config.yaml` nella directory di installazione.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configurazione principale di AgentOS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOsConfig {
    /// Configurazione del router LLM
    pub llm: LlmConfig,
    /// Configurazione Ollama (locale)
    pub ollama: OllamaConfig,
    /// Configurazione Claude API
    #[serde(default)]
    pub claude: ClaudeConfig,
    /// Configurazione OpenAI API
    #[serde(default)]
    pub openai: OpenAiConfig,
    /// Regole di sicurezza
    pub security: SecurityConfig,
    /// Configurazione sandbox
    pub sandbox: SandboxConfig,
    /// Comportamento dell'agente
    pub behavior: BehaviorConfig,
    /// Configurazione agent-fs
    #[serde(default)]
    pub fs: FsConfig,
    /// Configurazione agent-shell
    #[serde(default)]
    pub shell: ShellConfig,
    /// Configurazione MCP (Model Context Protocol) — server esterni di tool
    #[serde(default)]
    pub mcp: McpConfig,
    /// Configurazione connettori email e calendario (Google, Microsoft)
    #[serde(default)]
    pub connectors: ConnectorsConfig,
}

/// Configurazione di un singolo server MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Nome identificativo del server (es. "github", "slack")
    pub name: String,
    /// URL dell'endpoint MCP (HTTP POST)
    pub url: String,
    /// Header aggiuntivi per l'autenticazione (es. X-Api-Key)
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Configurazione MCP — lista di server esterni che espongono tool via JSON-RPC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// Lista dei server MCP configurati
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

/// Configurazione del router LLM — scelta dei modelli per ogni livello.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Backend predefinito per richieste normali
    pub default_backend: String,
    /// Backend per richieste complesse (pianificazione, ragionamento)
    pub complex_backend: String,
    /// Backend di fallback se i primi due falliscono
    pub fallback_backend: String,
    /// Timeout massimo per una richiesta LLM (secondi)
    #[serde(default = "default_llm_timeout")]
    pub timeout_secs: u64,
}

/// Configurazione per Ollama (LLM locale).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// URL del server Ollama
    #[serde(default = "default_ollama_url")]
    pub url: String,
    /// Modello per chat
    #[serde(default = "default_ollama_model")]
    pub model: String,
    /// Modello per embedding
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
}

/// Configurazione per Claude API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeConfig {
    /// API key (può essere letta da env ANTHROPIC_API_KEY)
    #[serde(default)]
    pub api_key: String,
    /// Modello da usare
    #[serde(default = "default_claude_model")]
    pub model: String,
    /// Massimo token in risposta
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

/// Configurazione per OpenAI API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAiConfig {
    /// API key (può essere letta da env OPENAI_API_KEY)
    #[serde(default)]
    pub api_key: String,
    /// Modello da usare
    #[serde(default = "default_openai_model")]
    pub model: String,
    /// Massimo token in risposta
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

/// Regole di sicurezza configurabili.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Pattern aggiuntivi per zona gialla (regex)
    #[serde(default)]
    pub yellow_patterns: Vec<PatternRule>,
    /// Comandi nella whitelist (esecuzione diretta senza sandbox)
    #[serde(default = "default_whitelist")]
    pub command_whitelist: Vec<String>,
    /// Se true, tutti i comandi non in whitelist vanno in sandbox
    #[serde(default = "default_true")]
    pub sandbox_by_default: bool,
}

/// Una regola pattern per la classificazione dei comandi.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternRule {
    /// Pattern regex da matchare
    pub pattern: String,
    /// Descrizione per l'utente
    pub description: String,
}

/// Configurazione della sandbox (bubblewrap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Timeout massimo per un comando in sandbox (secondi)
    #[serde(default = "default_sandbox_timeout")]
    pub timeout_secs: u64,
    /// Cartelle con accesso in scrittura dentro la sandbox
    #[serde(default)]
    pub writable_paths: Vec<String>,
    /// Se true, la rete è disabilitata in sandbox
    #[serde(default = "default_true")]
    pub disable_network: bool,
}

/// Comportamento dell'agente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// Lingua dell'agente (it, en)
    #[serde(default = "default_language")]
    pub language: String,
    /// Se true, l'agente spiega cosa sta facendo prima di farlo
    #[serde(default = "default_true")]
    pub explain_before_execute: bool,
    /// Numero massimo di comandi in un singolo piano
    #[serde(default = "default_max_plan_steps")]
    pub max_plan_steps: u32,
    /// Se true, mostra il briefing al login
    #[serde(default = "default_true")]
    pub show_briefing_on_login: bool,
}

/// Configurazione agent-fs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsConfig {
    /// Directory da monitorare
    #[serde(default = "default_watch_paths")]
    pub watch_paths: Vec<String>,
    /// Estensioni file da ignorare
    #[serde(default = "default_ignore_extensions")]
    pub ignore_extensions: Vec<String>,
    /// Directory da ignorare (glob pattern)
    #[serde(default = "default_ignore_dirs")]
    pub ignore_dirs: Vec<String>,
    /// Dimensione chunk per l'indicizzazione (token approssimativi)
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
    /// Punto di mount FUSE
    #[serde(default = "default_mount_point")]
    pub mount_point: String,
}

/// Configurazione agent-shell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Larghezza dell'area conversazione (percentuale dello schermo)
    #[serde(default = "default_conversation_width")]
    pub conversation_width_pct: u32,
    /// Terminale predefinito
    #[serde(default = "default_terminal")]
    pub terminal: String,
    /// Modalità workspace iniziale
    #[serde(default)]
    pub default_workspace_mode: WorkspaceModeConfig,
    /// Durata notifiche (millisecondi)
    #[serde(default = "default_notification_duration")]
    pub notification_duration_ms: u64,
}

/// Modalità workspace per la config (separata da WorkspaceMode per Default).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceModeConfig {
    #[default]
    Focus,
    Split,
    Canvas,
}

// ============================================================
// Configurazione connettori (email e calendario)
// ============================================================

/// Configurazione dei connettori per servizi esterni (email, calendario).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorsConfig {
    /// Configurazione Google (Gmail + Calendar)
    #[serde(default)]
    pub google: Option<GoogleConfig>,
    /// Configurazione Microsoft (Outlook + Calendar)
    #[serde(default)]
    pub microsoft: Option<MicrosoftConfig>,
}

/// Credenziali OAuth per Google (tipo "desktop app" nella Google Cloud Console).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    /// Client ID OAuth (tipo "Desktop app")
    pub client_id: String,
    /// Client secret OAuth
    pub client_secret: String,
    /// Refresh token (ottenuto dopo il primo /connect google)
    #[serde(default)]
    pub refresh_token: String,
}

/// Credenziali OAuth per Microsoft (App registration su Azure AD).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftConfig {
    /// Application (client) ID dall'Azure portal
    pub client_id: String,
    /// Tenant ID (o "common" per multi-tenant)
    #[serde(default = "default_ms_tenant")]
    pub tenant_id: String,
    /// Refresh token (ottenuto dopo il primo /connect outlook)
    #[serde(default)]
    pub refresh_token: String,
}

fn default_ms_tenant() -> String { "common".to_string() }

// ============================================================
// Valori di default
// ============================================================

fn default_ollama_url() -> String { "http://localhost:11434".to_string() }
fn default_ollama_model() -> String { "llama3.2".to_string() }
fn default_embedding_model() -> String { "nomic-embed-text".to_string() }
fn default_claude_model() -> String { "claude-sonnet-4-20250514".to_string() }
fn default_openai_model() -> String { "gpt-4o".to_string() }
fn default_max_tokens() -> u32 { 4096 }
fn default_llm_timeout() -> u64 { 120 }
fn default_sandbox_timeout() -> u64 { 30 }
fn default_language() -> String { "it".to_string() }
fn default_max_plan_steps() -> u32 { 10 }
fn default_true() -> bool { true }
fn default_chunk_size() -> usize { 500 }
fn default_mount_point() -> String { "/agent-fs".to_string() }
fn default_conversation_width() -> u32 { 30 }
fn default_terminal() -> String { "foot".to_string() }
fn default_notification_duration() -> u64 { 5000 }

fn default_whitelist() -> Vec<String> {
    vec![
        "ls", "cat", "pwd", "date", "whoami", "hostname", "uname",
        "head", "tail", "wc", "sort", "uniq", "grep", "find",
        "echo", "printf", "true", "false", "test",
        "df", "du", "free", "uptime", "top",
        "file", "stat", "realpath", "basename", "dirname",
    ].into_iter().map(String::from).collect()
}

fn default_watch_paths() -> Vec<String> {
    vec!["/home".to_string()]
}

fn default_ignore_extensions() -> Vec<String> {
    vec![
        ".o", ".so", ".a", ".pyc", ".class",
        ".swp", ".swo", ".tmp", ".lock",
    ].into_iter().map(String::from).collect()
}

fn default_ignore_dirs() -> Vec<String> {
    vec![
        ".git", ".cache", ".local/share/Trash",
        "node_modules", "__pycache__", ".venv",
        "target", "build", "dist",
    ].into_iter().map(String::from).collect()
}

// ============================================================
// Default per FsConfig e ShellConfig
// ============================================================

impl Default for FsConfig {
    fn default() -> Self {
        Self {
            watch_paths: default_watch_paths(),
            ignore_extensions: default_ignore_extensions(),
            ignore_dirs: default_ignore_dirs(),
            chunk_size: default_chunk_size(),
            mount_point: default_mount_point(),
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            conversation_width_pct: default_conversation_width(),
            terminal: default_terminal(),
            default_workspace_mode: WorkspaceModeConfig::default(),
            notification_duration_ms: default_notification_duration(),
        }
    }
}

// ============================================================
// Caricamento configurazione
// ============================================================

/// Errori nel caricamento della configurazione.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Errore di lettura del file: {0}")]
    Io(#[from] std::io::Error),

    #[error("Errore di parsing YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl AgentOsConfig {
    /// Carica la configurazione da un file YAML.
    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Carica la configurazione, sostituendo le API key con le variabili d'ambiente
    /// se presenti (ANTHROPIC_API_KEY, OPENAI_API_KEY).
    pub fn from_file_with_env(path: &str) -> Result<Self, ConfigError> {
        let mut config = Self::from_file(path)?;

        // Sovrascrivi API key da variabili d'ambiente se presenti
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            config.claude.api_key = key;
        }
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            config.openai.api_key = key;
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONFIG_YAML: &str = r#"
llm:
  default_backend: ollama
  complex_backend: claude
  fallback_backend: ollama
  timeout_secs: 120

ollama:
  url: "http://localhost:11434"
  model: "llama3.2"
  embedding_model: "nomic-embed-text"

claude:
  api_key: "test-key"
  model: "claude-sonnet-4-20250514"
  max_tokens: 4096

openai:
  api_key: ""
  model: "gpt-4o"
  max_tokens: 4096

security:
  yellow_patterns:
    - pattern: "^sudo\\s+"
      description: "Comandi con privilegi elevati"
    - pattern: "^apt\\s+(install|remove|purge)"
      description: "Gestione pacchetti"
  command_whitelist:
    - ls
    - cat
    - pwd
    - date
  sandbox_by_default: true

sandbox:
  timeout_secs: 30
  writable_paths:
    - "/tmp"
  disable_network: true

behavior:
  language: "it"
  explain_before_execute: true
  max_plan_steps: 10
  show_briefing_on_login: true
"#;

    #[test]
    fn test_parse_config() {
        let config: AgentOsConfig = serde_yaml::from_str(TEST_CONFIG_YAML).unwrap();

        assert_eq!(config.llm.default_backend, "ollama");
        assert_eq!(config.llm.complex_backend, "claude");
        assert_eq!(config.ollama.model, "llama3.2");
        assert_eq!(config.claude.api_key, "test-key");
        assert_eq!(config.security.yellow_patterns.len(), 2);
        assert!(config.security.sandbox_by_default);
        assert_eq!(config.behavior.language, "it");
        assert!(config.sandbox.disable_network);
    }

    #[test]
    fn test_serialize_config() {
        let config: AgentOsConfig = serde_yaml::from_str(TEST_CONFIG_YAML).unwrap();
        let yaml = serde_yaml::to_string(&config).unwrap();

        // Verifica che la serializzazione produca YAML valido
        let _reparsed: AgentOsConfig = serde_yaml::from_str(&yaml).unwrap();
    }

    #[test]
    fn test_default_whitelist() {
        let whitelist = default_whitelist();
        assert!(whitelist.contains(&"ls".to_string()));
        assert!(whitelist.contains(&"cat".to_string()));
        assert!(whitelist.contains(&"pwd".to_string()));
    }

    #[test]
    fn test_fs_config_defaults() {
        let fs = FsConfig::default();
        assert_eq!(fs.chunk_size, 500);
        assert_eq!(fs.mount_point, "/agent-fs");
        assert!(fs.ignore_dirs.contains(&".git".to_string()));
    }

    #[test]
    fn test_shell_config_defaults() {
        let shell = ShellConfig::default();
        assert_eq!(shell.conversation_width_pct, 30);
        assert_eq!(shell.terminal, "foot");
        assert_eq!(shell.notification_duration_ms, 5000);
    }

    #[test]
    fn test_config_missing_optional_sections() {
        // fs, shell e mcp hanno Default, quindi non servono nel YAML
        let config: AgentOsConfig = serde_yaml::from_str(TEST_CONFIG_YAML).unwrap();
        assert_eq!(config.fs.chunk_size, 500); // default
        assert_eq!(config.shell.terminal, "foot"); // default
        assert!(config.mcp.servers.is_empty()); // default: nessun server MCP
    }

    #[test]
    fn test_mcp_config_parsing() {
        let yaml = format!("{}\nmcp:\n  servers:\n    - name: example\n      url: \"https://example.com/mcp\"\n      headers:\n        X-Api-Key: \"key123\"\n", TEST_CONFIG_YAML);
        let config: AgentOsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.mcp.servers.len(), 1);
        assert_eq!(config.mcp.servers[0].name, "example");
        assert_eq!(config.mcp.servers[0].url, "https://example.com/mcp");
        assert_eq!(config.mcp.servers[0].headers.get("X-Api-Key").unwrap(), "key123");
    }

    #[test]
    fn test_connectors_config_default() {
        // Senza sezione connectors nel YAML, deve usare i default (None)
        let config: AgentOsConfig = serde_yaml::from_str(TEST_CONFIG_YAML).unwrap();
        assert!(config.connectors.google.is_none());
        assert!(config.connectors.microsoft.is_none());
    }

    #[test]
    fn test_connectors_config_google() {
        let yaml = format!("{}\nconnectors:\n  google:\n    client_id: \"test-client-id\"\n    client_secret: \"test-secret\"\n", TEST_CONFIG_YAML);
        let config: AgentOsConfig = serde_yaml::from_str(&yaml).unwrap();
        let google = config.connectors.google.unwrap();
        assert_eq!(google.client_id, "test-client-id");
        assert_eq!(google.client_secret, "test-secret");
        assert!(google.refresh_token.is_empty());
        assert!(config.connectors.microsoft.is_none());
    }

    #[test]
    fn test_connectors_config_microsoft() {
        let yaml = format!("{}\nconnectors:\n  microsoft:\n    client_id: \"ms-client-id\"\n    tenant_id: \"my-tenant\"\n", TEST_CONFIG_YAML);
        let config: AgentOsConfig = serde_yaml::from_str(&yaml).unwrap();
        let ms = config.connectors.microsoft.unwrap();
        assert_eq!(ms.client_id, "ms-client-id");
        assert_eq!(ms.tenant_id, "my-tenant");
    }

    #[test]
    fn test_connectors_config_ms_default_tenant() {
        let yaml = format!("{}\nconnectors:\n  microsoft:\n    client_id: \"ms-client-id\"\n", TEST_CONFIG_YAML);
        let config: AgentOsConfig = serde_yaml::from_str(&yaml).unwrap();
        let ms = config.connectors.microsoft.unwrap();
        assert_eq!(ms.tenant_id, "common");
    }
}
