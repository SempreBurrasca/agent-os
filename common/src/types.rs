//! Tipi condivisi tra tutti i componenti di AgentOS.

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Zona di rischio di un comando.
/// Determina se il comando può essere eseguito automaticamente o richiede conferma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskZone {
    /// Sicuro — esecuzione automatica (ls, cat, pwd, date...)
    Green,
    /// Attenzione — richiede conferma dell'utente (apt install, mv, cp...)
    Yellow,
    /// Pericoloso — bloccato dal Guardian (rm -rf /, fork bomb, dd su device...)
    Red,
}

/// Modalità di layout del workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
    /// La finestra attiva prende tutto lo spazio
    Focus,
    /// Due finestre affiancate con divisore
    Split,
    /// Finestre libere posizionabili
    Canvas,
}

/// Livello di urgenza per le notifiche.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low,
    Normal,
    High,
    Critical,
}

/// Verdetto del Guardian su un comando.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianVerdict {
    /// Zona di rischio assegnata
    pub zone: RiskZone,
    /// Motivazione del verdetto
    pub reason: String,
    /// Il comando valutato
    pub command: String,
    /// Se true, il comando è stato bloccato (zona rossa)
    pub blocked: bool,
}

/// Risultato dell'esecuzione di un comando.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Il comando eseguito
    pub command: String,
    /// Output standard
    pub stdout: String,
    /// Output errore
    pub stderr: String,
    /// Codice di ritorno del processo
    pub return_code: i32,
    /// Se il comando è stato eseguito in sandbox (bwrap)
    pub sandboxed: bool,
    /// Se il comando è stato interrotto per timeout
    pub timed_out: bool,
}

/// Riepilogo di un'email per il briefing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummary {
    pub from: String,
    pub subject: String,
    pub preview: String,
    pub received_at: DateTime<Utc>,
    pub unread: bool,
}

/// Evento calendario per il briefing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub location: Option<String>,
}

/// File recente per il briefing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    pub modified_at: DateTime<Utc>,
    pub size_bytes: u64,
}

/// Azione disponibile in una notifica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifAction {
    /// Identificativo unico dell'azione
    pub id: String,
    /// Etichetta mostrata all'utente
    pub label: String,
}

/// Posizionamento di una finestra nel workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// ID della finestra Wayland
    pub window_id: u64,
    /// Posizione X in pixel
    pub x: i32,
    /// Posizione Y in pixel
    pub y: i32,
    /// Larghezza in pixel
    pub width: u32,
    /// Altezza in pixel
    pub height: u32,
}

/// Risultato di una ricerca semantica in agent-fs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Percorso del file trovato
    pub path: String,
    /// Nome del file
    pub name: String,
    /// Snippet del contenuto rilevante
    pub snippet: String,
    /// Punteggio di rilevanza (0.0 - 1.0)
    pub score: f64,
    /// Tipo di file (pdf, txt, code, image...)
    pub file_type: String,
    /// Data ultima modifica
    pub modified_at: DateTime<Utc>,
}

/// Intento riconosciuto dall'Intent Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedIntent {
    /// Se l'agente ha capito la richiesta
    pub understood: bool,
    /// Tipo di intento (file_operation, system_command, search, question...)
    pub intent: String,
    /// Comandi da eseguire (se applicabile)
    pub commands: Vec<String>,
    /// Spiegazione per l'utente
    pub explanation: String,
    /// Se serve interazione aggiuntiva con l'utente
    pub needs_interaction: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_zone_serialization() {
        let green = RiskZone::Green;
        let json = serde_json::to_string(&green).unwrap();
        assert_eq!(json, "\"green\"");

        let deserialized: RiskZone = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, RiskZone::Green);
    }

    #[test]
    fn test_guardian_verdict_serialization() {
        let verdict = GuardianVerdict {
            zone: RiskZone::Red,
            reason: "Comando distruttivo bloccato".to_string(),
            command: "rm -rf /".to_string(),
            blocked: true,
        };

        let json = serde_json::to_string(&verdict).unwrap();
        let deserialized: GuardianVerdict = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.zone, RiskZone::Red);
        assert!(deserialized.blocked);
        assert_eq!(deserialized.command, "rm -rf /");
    }

    #[test]
    fn test_execution_result_serialization() {
        let result = ExecutionResult {
            command: "ls -la".to_string(),
            stdout: "total 0\n".to_string(),
            stderr: String::new(),
            return_code: 0,
            sandboxed: false,
            timed_out: false,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ExecutionResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.return_code, 0);
        assert!(!deserialized.sandboxed);
    }

    #[test]
    fn test_parsed_intent_serialization() {
        let intent = ParsedIntent {
            understood: true,
            intent: "file_operation".to_string(),
            commands: vec!["ls -la".to_string()],
            explanation: "Mostro i file nella directory corrente".to_string(),
            needs_interaction: false,
        };

        let json = serde_json::to_string(&intent).unwrap();
        let deserialized: ParsedIntent = serde_json::from_str(&json).unwrap();

        assert!(deserialized.understood);
        assert_eq!(deserialized.commands.len(), 1);
    }

    #[test]
    fn test_workspace_mode_serialization() {
        let modes = vec![WorkspaceMode::Focus, WorkspaceMode::Split, WorkspaceMode::Canvas];
        for mode in &modes {
            let json = serde_json::to_string(mode).unwrap();
            let deserialized: WorkspaceMode = serde_json::from_str(&json).unwrap();
            assert_eq!(*mode, deserialized);
        }
    }

    #[test]
    fn test_search_result() {
        let result = SearchResult {
            path: "/home/user/fattura.pdf".to_string(),
            name: "fattura.pdf".to_string(),
            snippet: "Fattura n. 42 - Studio dentistico".to_string(),
            score: 0.87,
            file_type: "pdf".to_string(),
            modified_at: Utc::now(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.score, 0.87);
    }
}
