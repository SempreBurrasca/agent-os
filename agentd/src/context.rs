//! Context Manager — salvataggio e ripristino dello stato sessione.
//!
//! Serializza lo stato in JSON su disco (~/.agentd/session.json).
//! Chiamato allo shutdown per salvare e al boot per ripristinare.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn, debug};

/// Stato della sessione da persistere su disco.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    /// Directory corrente dell'utente
    pub current_dir: String,
    /// Ultimi comandi eseguiti (massimo 50)
    pub recent_commands: Vec<String>,
    /// Conversazione in corso (ultimi messaggi)
    pub conversation: Vec<ConversationEntry>,
    /// Finestre aperte (ricevute da agent-shell via IPC)
    pub open_windows: Vec<WindowInfo>,
    /// Timestamp dell'ultimo salvataggio
    pub last_saved: String,
}

/// Singola entry nella conversazione.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    pub role: String,     // "user" o "assistant"
    pub content: String,
    pub timestamp: String,
}

/// Informazioni su una finestra aperta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u64,
    pub app_name: String,
    pub title: String,
}

/// Context Manager — gestisce il salvataggio/ripristino dello stato.
pub struct ContextManager {
    /// Percorso del file di stato
    state_path: PathBuf,
    /// Stato corrente
    state: SessionState,
    /// Numero massimo di comandi recenti da conservare
    max_recent_commands: usize,
    /// Numero massimo di messaggi conversazione da conservare
    max_conversation_entries: usize,
}

impl ContextManager {
    /// Crea un nuovo ContextManager. Crea la directory se necessario.
    pub fn new(state_dir: &str) -> Result<Self> {
        let state_dir = Path::new(state_dir);
        if !state_dir.exists() {
            std::fs::create_dir_all(state_dir)?;
        }

        let state_path = state_dir.join("session.json");

        let mut manager = Self {
            state_path,
            state: SessionState::default(),
            max_recent_commands: 50,
            max_conversation_entries: 100,
        };

        // Prova a ripristinare lo stato precedente
        manager.restore_context()?;

        Ok(manager)
    }

    /// Salva il contesto su disco.
    pub fn save_context(&mut self) -> Result<()> {
        self.state.last_saved = chrono::Utc::now().to_rfc3339();

        let json = serde_json::to_string_pretty(&self.state)?;
        std::fs::write(&self.state_path, json)?;

        debug!(path = %self.state_path.display(), "Contesto salvato");
        Ok(())
    }

    /// Ripristina il contesto da disco.
    pub fn restore_context(&mut self) -> Result<()> {
        if self.state_path.exists() {
            let content = std::fs::read_to_string(&self.state_path)?;
            match serde_json::from_str::<SessionState>(&content) {
                Ok(state) => {
                    info!(last_saved = %state.last_saved, "Contesto ripristinato");
                    self.state = state;
                }
                Err(e) => {
                    warn!(error = %e, "Errore parsing del contesto salvato — reset");
                    self.state = SessionState::default();
                }
            }
        } else {
            debug!("Nessun contesto precedente trovato");
        }
        Ok(())
    }

    /// Aggiorna la directory corrente.
    pub fn set_current_dir(&mut self, dir: &str) {
        self.state.current_dir = dir.to_string();
    }

    /// Restituisce la directory corrente.
    pub fn current_dir(&self) -> &str {
        &self.state.current_dir
    }

    /// Aggiunge un comando alla lista dei recenti.
    pub fn add_command(&mut self, command: &str) {
        self.state.recent_commands.push(command.to_string());
        if self.state.recent_commands.len() > self.max_recent_commands {
            self.state.recent_commands.remove(0);
        }
    }

    /// Restituisce gli ultimi N comandi.
    pub fn recent_commands(&self, n: usize) -> &[String] {
        let len = self.state.recent_commands.len();
        let start = len.saturating_sub(n);
        &self.state.recent_commands[start..]
    }

    /// Aggiunge un messaggio alla conversazione.
    pub fn add_conversation_entry(&mut self, role: &str, content: &str) {
        let entry = ConversationEntry {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.state.conversation.push(entry);

        if self.state.conversation.len() > self.max_conversation_entries {
            self.state.conversation.remove(0);
        }
    }

    /// Restituisce la conversazione corrente.
    pub fn conversation(&self) -> &[ConversationEntry] {
        &self.state.conversation
    }

    /// Aggiorna la lista delle finestre aperte.
    pub fn update_windows(&mut self, windows: Vec<WindowInfo>) {
        self.state.open_windows = windows;
    }

    /// Restituisce le finestre aperte.
    pub fn open_windows(&self) -> &[WindowInfo] {
        &self.state.open_windows
    }

    /// Resetta completamente lo stato.
    pub fn reset(&mut self) {
        self.state = SessionState::default();
    }

    /// Genera un riepilogo leggibile della sessione precedente.
    /// Restituisce None se non c'è stato precedente (prima sessione).
    pub fn generate_session_summary(&self) -> Option<String> {
        if self.state.last_saved.is_empty() && self.state.conversation.is_empty() {
            return None;
        }

        let mut parts = Vec::new();

        // Timestamp ultimo salvataggio
        if !self.state.last_saved.is_empty() {
            parts.push(format!("Ultima sessione salvata: {}", self.state.last_saved));
        }

        // Directory di lavoro
        if !self.state.current_dir.is_empty() {
            parts.push(format!("Directory di lavoro: {}", self.state.current_dir));
        }

        // Ultimi comandi eseguiti
        if !self.state.recent_commands.is_empty() {
            let last_cmds: Vec<&str> = self.state.recent_commands.iter()
                .rev().take(5)
                .map(|s| s.as_str())
                .collect();
            parts.push(format!("Ultimi comandi: {}", last_cmds.join(", ")));
        }

        // Numero di messaggi nella conversazione
        if !self.state.conversation.is_empty() {
            parts.push(format!("Messaggi nella conversazione: {}", self.state.conversation.len()));
            // Ultimo argomento trattato
            if let Some(last_user) = self.state.conversation.iter().rev()
                .find(|e| e.role == "user")
            {
                let preview = if last_user.content.len() > 80 {
                    format!("{}...", &last_user.content[..80])
                } else {
                    last_user.content.clone()
                };
                parts.push(format!("Ultimo argomento: \"{}\"", preview));
            }
        }

        // Finestre aperte
        if !self.state.open_windows.is_empty() {
            let windows: Vec<String> = self.state.open_windows.iter()
                .map(|w| format!("{} ({})", w.app_name, w.title))
                .collect();
            parts.push(format!("Finestre aperte: {}", windows.join(", ")));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_context() -> (ContextManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let manager = ContextManager::new(dir.path().to_str().unwrap()).unwrap();
        (manager, dir)
    }

    #[test]
    fn test_new_context() {
        let (ctx, _dir) = test_context();
        assert_eq!(ctx.current_dir(), "");
        assert!(ctx.recent_commands(10).is_empty());
    }

    #[test]
    fn test_set_current_dir() {
        let (mut ctx, _dir) = test_context();
        ctx.set_current_dir("/home/user/projects");
        assert_eq!(ctx.current_dir(), "/home/user/projects");
    }

    #[test]
    fn test_add_commands() {
        let (mut ctx, _dir) = test_context();
        ctx.add_command("ls -la");
        ctx.add_command("pwd");
        ctx.add_command("cat README.md");

        let recent = ctx.recent_commands(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0], "pwd");
        assert_eq!(recent[1], "cat README.md");
    }

    #[test]
    fn test_recent_commands_limit() {
        let (mut ctx, _dir) = test_context();
        for i in 0..60 {
            ctx.add_command(&format!("command {}", i));
        }
        // Deve mantenere solo 50 comandi
        assert_eq!(ctx.state.recent_commands.len(), 50);
    }

    #[test]
    fn test_conversation() {
        let (mut ctx, _dir) = test_context();
        ctx.add_conversation_entry("user", "mostrami i file");
        ctx.add_conversation_entry("assistant", "Ecco i file nella directory:");

        let conv = ctx.conversation();
        assert_eq!(conv.len(), 2);
        assert_eq!(conv[0].role, "user");
        assert_eq!(conv[1].role, "assistant");
    }

    #[test]
    fn test_save_and_restore() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_str().unwrap().to_string();

        // Salva lo stato
        {
            let mut ctx = ContextManager::new(&dir_path).unwrap();
            ctx.set_current_dir("/home/user");
            ctx.add_command("ls -la");
            ctx.add_conversation_entry("user", "ciao");
            ctx.save_context().unwrap();
        }

        // Ripristina in una nuova istanza
        {
            let ctx = ContextManager::new(&dir_path).unwrap();
            assert_eq!(ctx.current_dir(), "/home/user");
            assert_eq!(ctx.recent_commands(10).len(), 1);
            assert_eq!(ctx.conversation().len(), 1);
        }
    }

    #[test]
    fn test_update_windows() {
        let (mut ctx, _dir) = test_context();
        ctx.update_windows(vec![
            WindowInfo {
                window_id: 1,
                app_name: "foot".to_string(),
                title: "Terminal".to_string(),
            },
        ]);
        assert_eq!(ctx.open_windows().len(), 1);
        assert_eq!(ctx.open_windows()[0].app_name, "foot");
    }

    #[test]
    fn test_reset() {
        let (mut ctx, _dir) = test_context();
        ctx.set_current_dir("/home/user");
        ctx.add_command("ls");
        ctx.reset();
        assert_eq!(ctx.current_dir(), "");
        assert!(ctx.recent_commands(10).is_empty());
    }
}
