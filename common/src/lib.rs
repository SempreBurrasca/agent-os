//! agentos-common — Tipi condivisi e protocollo IPC per AgentOS
//!
//! Questo crate contiene tutte le definizioni di tipo, i messaggi IPC
//! e la configurazione usati dai tre componenti di AgentOS:
//! agentd, agent-shell, agent-fs.

pub mod config;
pub mod ipc;
pub mod types;

// Re-export dei tipi più usati per comodità
pub use config::AgentOsConfig;
pub use ipc::{AgentToShell, ShellToAgent, FsToAgent, AgentToFs};
pub use types::{RiskZone, WorkspaceMode, Urgency};
