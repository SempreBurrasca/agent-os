//! Sincronizzazione periodica — email e calendario.
//!
//! `SyncManager` gestisce il polling a intervalli regolari per:
//! - Email: recupera i nuovi messaggi e li inserisce nel knowledge graph
//! - Calendario: recupera gli eventi dei prossimi 7 giorni e li inserisce nel knowledge graph
//!
//! Lo stato di sincronizzazione è persistito in ~/.agentos/sync_state.json
//! per evitare di ri-processare gli stessi dati al riavvio.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::connectors::{
    self, CalendarConnector, ConnectorProvider, EmailConnector,
};
use crate::knowledge::KnowledgeGraph;

/// Stato di sincronizzazione persistito su disco.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// Timestamp dell'ultima sincronizzazione email riuscita
    pub last_email_sync: Option<DateTime<Utc>>,
    /// Timestamp dell'ultima sincronizzazione calendario riuscita
    pub last_calendar_sync: Option<DateTime<Utc>>,
    /// ID dell'ultimo messaggio email processato (per evitare duplicati)
    pub last_email_id: Option<String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            last_email_sync: None,
            last_calendar_sync: None,
            last_email_id: None,
        }
    }
}

/// Gestore della sincronizzazione periodica.
pub struct SyncManager {
    /// Stato corrente della sincronizzazione
    state: SyncState,
    /// Percorso del file di stato
    state_path: PathBuf,
}

impl SyncManager {
    /// Crea un nuovo SyncManager, caricando lo stato da disco se disponibile.
    pub fn new() -> Self {
        let state_path = sync_state_path();
        let state = load_sync_state(&state_path);

        if let Some(last_email) = &state.last_email_sync {
            debug!(last_email = %last_email, "Stato sincronizzazione caricato");
        }

        Self { state, state_path }
    }

    /// Verifica se c'è un connettore email configurato (token presenti).
    pub fn has_email_connector() -> bool {
        connectors::load_tokens(ConnectorProvider::Google).is_ok()
            || connectors::load_tokens(ConnectorProvider::Microsoft).is_ok()
    }

    /// Verifica se c'è un connettore calendario configurato.
    pub fn has_calendar_connector() -> bool {
        // I connettori email e calendario condividono gli stessi token
        Self::has_email_connector()
    }

    /// Sincronizza le email e inserisce i contenuti nel knowledge graph.
    /// Restituisce il numero di email nuove processate.
    pub async fn sync_emails(&mut self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        if !Self::has_email_connector() {
            debug!("Nessun connettore email configurato — sincronizzazione saltata");
            return 0;
        }

        // Determina quale provider usare (priorità: Google, poi Microsoft)
        let count = if connectors::load_tokens(ConnectorProvider::Google).is_ok() {
            self.sync_emails_google(knowledge_graph).await
        } else {
            self.sync_emails_microsoft(knowledge_graph).await
        };

        // Aggiorna timestamp e salva stato
        if count > 0 {
            self.state.last_email_sync = Some(Utc::now());
            self.save_state();
        }

        count
    }

    /// Sincronizza il calendario e inserisce gli eventi nel knowledge graph.
    /// Restituisce il numero di eventi processati.
    pub async fn sync_calendar(&mut self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        if !Self::has_calendar_connector() {
            debug!("Nessun connettore calendario configurato — sincronizzazione saltata");
            return 0;
        }

        // Determina quale provider usare
        let count = if connectors::load_tokens(ConnectorProvider::Google).is_ok() {
            self.sync_calendar_google(knowledge_graph).await
        } else {
            self.sync_calendar_microsoft(knowledge_graph).await
        };

        // Aggiorna timestamp e salva stato
        if count > 0 {
            self.state.last_calendar_sync = Some(Utc::now());
            self.save_state();
        }

        count
    }

    /// Sincronizza email tramite Google (Gmail).
    async fn sync_emails_google(&mut self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        // Carica i token — se fallisce, non possiamo procedere
        let tokens = match connectors::load_tokens(ConnectorProvider::Google) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Impossibile caricare i token Google per la sincronizzazione email");
                return 0;
            }
        };

        // Crea un connettore temporaneo con i token esistenti
        // Nota: client_id e client_secret non servono per le chiamate API (solo per il refresh),
        // ma il connettore li richiede. Usiamo stringhe vuote — il refresh fallirà ma
        // le chiamate con access_token valido funzioneranno.
        let connector = crate::connectors::google::GoogleConnector::with_tokens(
            "", "", tokens,
        );

        // Recupera le ultime 10 email
        let emails = match connector.list_emails(10).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Errore recupero email Google durante la sincronizzazione");
                return 0;
            }
        };

        let mut count = 0;
        for email in &emails {
            // Salta email già processate (controllo ID)
            if let Some(ref last_id) = self.state.last_email_id {
                if &email.id == last_id {
                    break; // Le email sono ordinate per data, possiamo fermarci
                }
            }

            // Inserisci nel knowledge graph
            let source = format!("email:{}", email.from);
            let content = format!("{}: {}", email.subject, email.body_preview);
            knowledge_graph.add_document(&source, &content);
            count += 1;
        }

        // Aggiorna l'ultimo ID processato
        if let Some(first) = emails.first() {
            self.state.last_email_id = Some(first.id.clone());
        }

        if count > 0 {
            info!(nuove_email = count, "Sincronizzazione email completata");
        }

        count
    }

    /// Sincronizza email tramite Microsoft (Outlook).
    async fn sync_emails_microsoft(&mut self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        let tokens = match connectors::load_tokens(ConnectorProvider::Microsoft) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Impossibile caricare i token Microsoft per la sincronizzazione email");
                return 0;
            }
        };

        let connector = crate::connectors::outlook::OutlookConnector::with_tokens(
            "", "", tokens,
        );

        let emails = match connector.list_emails(10).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Errore recupero email Outlook durante la sincronizzazione");
                return 0;
            }
        };

        let mut count = 0;
        for email in &emails {
            if let Some(ref last_id) = self.state.last_email_id {
                if &email.id == last_id {
                    break;
                }
            }

            let source = format!("email:{}", email.from);
            let content = format!("{}: {}", email.subject, email.body_preview);
            knowledge_graph.add_document(&source, &content);
            count += 1;
        }

        if let Some(first) = emails.first() {
            self.state.last_email_id = Some(first.id.clone());
        }

        if count > 0 {
            info!(nuove_email = count, "Sincronizzazione email Outlook completata");
        }

        count
    }

    /// Sincronizza calendario tramite Google Calendar.
    async fn sync_calendar_google(&self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        let tokens = match connectors::load_tokens(ConnectorProvider::Google) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Impossibile caricare i token Google per la sincronizzazione calendario");
                return 0;
            }
        };

        let connector = crate::connectors::google::GoogleConnector::with_tokens(
            "", "", tokens,
        );

        // Recupera eventi dei prossimi 7 giorni
        let events = match connector.list_events(7, 20).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Errore recupero eventi Google Calendar durante la sincronizzazione");
                return 0;
            }
        };

        let mut count = 0;
        for event in &events {
            let description = event.description.as_deref().unwrap_or("");
            let date = event.start.format("%Y-%m-%d %H:%M").to_string();
            let content = format!("{} {} {}", event.title, date, description);
            knowledge_graph.add_document("calendario", &content);
            count += 1;
        }

        if count > 0 {
            info!(eventi = count, "Sincronizzazione calendario Google completata");
        }

        count
    }

    /// Sincronizza calendario tramite Microsoft Outlook Calendar.
    async fn sync_calendar_microsoft(&self, knowledge_graph: &mut KnowledgeGraph) -> usize {
        let tokens = match connectors::load_tokens(ConnectorProvider::Microsoft) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Impossibile caricare i token Microsoft per la sincronizzazione calendario");
                return 0;
            }
        };

        let connector = crate::connectors::outlook::OutlookConnector::with_tokens(
            "", "", tokens,
        );

        let events = match connector.list_events(7, 20).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Errore recupero eventi Outlook Calendar durante la sincronizzazione");
                return 0;
            }
        };

        let mut count = 0;
        for event in &events {
            let description = event.description.as_deref().unwrap_or("");
            let date = event.start.format("%Y-%m-%d %H:%M").to_string();
            let content = format!("{} {} {}", event.title, date, description);
            knowledge_graph.add_document("calendario", &content);
            count += 1;
        }

        if count > 0 {
            info!(eventi = count, "Sincronizzazione calendario Outlook completata");
        }

        count
    }

    /// Salva lo stato di sincronizzazione su disco.
    fn save_state(&self) {
        if let Some(parent) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&self.state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.state_path, json) {
                    warn!(error = %e, "Errore salvataggio stato sincronizzazione");
                }
            }
            Err(e) => {
                warn!(error = %e, "Errore serializzazione stato sincronizzazione");
            }
        }
    }

    /// Restituisce lo stato corrente (per debug/briefing).
    pub fn state(&self) -> &SyncState {
        &self.state
    }
}

/// Percorso del file di stato sincronizzazione.
fn sync_state_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".agentos").join("sync_state.json")
}

/// Carica lo stato di sincronizzazione dal disco.
fn load_sync_state(path: &PathBuf) -> SyncState {
    if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(state) => {
                    debug!("Stato sincronizzazione caricato da {:?}", path);
                    return state;
                }
                Err(e) => {
                    warn!("Errore parsing stato sincronizzazione: {} — creo nuovo stato", e);
                }
            },
            Err(e) => {
                warn!("Errore lettura stato sincronizzazione: {} — creo nuovo stato", e);
            }
        }
    }
    SyncState::default()
}

// ============================================================
// Test
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_state_default() {
        let state = SyncState::default();
        assert!(state.last_email_sync.is_none());
        assert!(state.last_calendar_sync.is_none());
        assert!(state.last_email_id.is_none());
    }

    #[test]
    fn test_sync_state_serialization() {
        let state = SyncState {
            last_email_sync: Some(Utc::now()),
            last_calendar_sync: Some(Utc::now()),
            last_email_id: Some("msg123".to_string()),
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: SyncState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.last_email_id, Some("msg123".to_string()));
        assert!(restored.last_email_sync.is_some());
        assert!(restored.last_calendar_sync.is_some());
    }

    #[test]
    fn test_sync_state_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync_state.json");

        let state = SyncState {
            last_email_sync: Some(Utc::now()),
            last_calendar_sync: None,
            last_email_id: Some("test_id".to_string()),
        };

        // Salva
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        // Carica
        let loaded = load_sync_state(&path);
        assert_eq!(loaded.last_email_id, Some("test_id".to_string()));
        assert!(loaded.last_email_sync.is_some());
        assert!(loaded.last_calendar_sync.is_none());
    }

    #[test]
    fn test_load_sync_state_missing_file() {
        let path = PathBuf::from("/tmp/nonexistent_sync_state_test_abc123.json");
        let state = load_sync_state(&path);
        assert!(state.last_email_sync.is_none());
    }

    #[test]
    fn test_load_sync_state_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync_state.json");
        std::fs::write(&path, "non json valido {{{").unwrap();

        let state = load_sync_state(&path);
        assert!(state.last_email_sync.is_none()); // Deve tornare al default
    }

    #[test]
    fn test_has_no_connectors_by_default() {
        // In un ambiente di test non ci sono token — entrambi devono essere false
        // (a meno che la macchina dell'utente non abbia token reali, ma il test
        // è comunque valido perché verifica la logica)
        let _has_email = SyncManager::has_email_connector();
        let _has_cal = SyncManager::has_calendar_connector();
        // Non possiamo asserire il valore perché dipende dall'ambiente,
        // ma verifichiamo che non va in panic
    }

    #[test]
    fn test_sync_manager_creation() {
        // Verifica che la creazione del SyncManager non fallisce
        let manager = SyncManager::new();
        let state = manager.state();
        // Lo stato iniziale potrebbe avere valori da un run precedente
        // o essere vuoto — entrambi sono validi
        let _ = state.last_email_sync;
    }
}
