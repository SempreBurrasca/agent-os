//! Connettori per servizi esterni (email e calendario).
//!
//! Fornisce trait comuni `EmailConnector` e `CalendarConnector` con
//! implementazioni per Google (Gmail + Calendar) e Microsoft (Outlook + Calendar).
//! L'autenticazione avviene tramite OAuth2 device code flow (adatto alla CLI).

pub mod google;
pub mod outlook;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

// ============================================================
// Tipi condivisi tra i connettori
// ============================================================

/// Tipo di connettore configurato.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorProvider {
    Google,
    Microsoft,
}

/// Messaggio email recuperato da un connettore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Email {
    /// ID univoco del messaggio (provider-specific)
    pub id: String,
    /// Mittente
    pub from: String,
    /// Destinatario/i
    pub to: Vec<String>,
    /// Oggetto
    pub subject: String,
    /// Anteprima del corpo (testo semplice, troncato)
    pub body_preview: String,
    /// Data di ricezione
    pub received_at: DateTime<Utc>,
    /// Se il messaggio è non letto
    pub unread: bool,
    /// Se ha allegati
    pub has_attachments: bool,
}

/// Evento calendario recuperato da un connettore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalEvent {
    /// ID univoco dell'evento (provider-specific)
    pub id: String,
    /// Titolo dell'evento
    pub title: String,
    /// Inizio
    pub start: DateTime<Utc>,
    /// Fine
    pub end: DateTime<Utc>,
    /// Luogo (opzionale)
    pub location: Option<String>,
    /// Descrizione (opzionale)
    pub description: Option<String>,
    /// Se è un evento di tutto il giorno
    pub all_day: bool,
}

/// Parametri per la creazione di un evento calendario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEventParams {
    /// Titolo dell'evento
    pub title: String,
    /// Inizio (ISO 8601)
    pub start: DateTime<Utc>,
    /// Fine (ISO 8601)
    pub end: DateTime<Utc>,
    /// Luogo (opzionale)
    pub location: Option<String>,
    /// Descrizione (opzionale)
    pub description: Option<String>,
}

/// Risposta del flusso OAuth2 device code (primo passo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    /// Codice dispositivo (interno, per il polling)
    pub device_code: String,
    /// Codice utente da mostrare all'utente
    pub user_code: String,
    /// URL dove l'utente deve andare per autorizzare
    pub verification_url: String,
    /// Intervallo di polling in secondi
    pub interval: u64,
    /// Scadenza in secondi
    pub expires_in: u64,
}

/// Token OAuth2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    /// Access token per le chiamate API
    pub access_token: String,
    /// Refresh token per ottenere nuovi access token
    pub refresh_token: String,
    /// Scadenza dell'access token
    pub expires_at: DateTime<Utc>,
}

impl OAuthTokens {
    /// Verifica se l'access token è scaduto (con margine di 60 secondi).
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at - chrono::Duration::seconds(60)
    }
}

/// Errore generico dei connettori.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("Errore HTTP: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Errore I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("Errore JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Token scaduto o non valido")]
    TokenExpired,

    #[error("Connettore non configurato: {0}")]
    NotConfigured(String),

    #[error("Errore OAuth: {0}")]
    OAuth(String),

    #[error("Errore API: {status} — {message}")]
    Api { status: u16, message: String },
}

// ============================================================
// Trait EmailConnector
// ============================================================

/// Trait per i connettori email (Gmail, Outlook).
#[async_trait]
pub trait EmailConnector: Send + Sync {
    /// Elenca le email recenti (ultime `max_results`).
    async fn list_emails(&self, max_results: u32) -> Result<Vec<Email>, ConnectorError>;

    /// Legge una singola email per ID.
    async fn read_email(&self, email_id: &str) -> Result<Email, ConnectorError>;

    /// Cerca email per query testuale.
    async fn search_emails(&self, query: &str, max_results: u32) -> Result<Vec<Email>, ConnectorError>;
}

// ============================================================
// Trait CalendarConnector
// ============================================================

/// Trait per i connettori calendario (Google Calendar, Outlook Calendar).
#[async_trait]
pub trait CalendarConnector: Send + Sync {
    /// Elenca i prossimi eventi (entro i prossimi `days` giorni).
    async fn list_events(&self, days: u32, max_results: u32) -> Result<Vec<CalEvent>, ConnectorError>;

    /// Crea un nuovo evento nel calendario.
    async fn create_event(&self, params: CreateEventParams) -> Result<CalEvent, ConnectorError>;
}

// ============================================================
// Gestione token su disco
// ============================================================

/// Directory dove vengono salvati i token OAuth.
fn tokens_dir() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    home.join(".agentos").join("tokens")
}

/// Salva i token OAuth su disco per un provider.
pub fn save_tokens(provider: ConnectorProvider, tokens: &OAuthTokens) -> Result<(), ConnectorError> {
    let dir = tokens_dir();
    std::fs::create_dir_all(&dir)?;

    let filename = match provider {
        ConnectorProvider::Google => "google.json",
        ConnectorProvider::Microsoft => "microsoft.json",
    };

    let path = dir.join(filename);
    let json = serde_json::to_string_pretty(tokens)?;
    std::fs::write(path, json)?;

    Ok(())
}

/// Carica i token OAuth dal disco per un provider.
pub fn load_tokens(provider: ConnectorProvider) -> Result<OAuthTokens, ConnectorError> {
    let dir = tokens_dir();
    let filename = match provider {
        ConnectorProvider::Google => "google.json",
        ConnectorProvider::Microsoft => "microsoft.json",
    };

    let path = dir.join(filename);
    if !path.exists() {
        return Err(ConnectorError::NotConfigured(
            format!("Token {} non trovati. Usa /connect per autenticarti.", filename)
        ));
    }

    let json = std::fs::read_to_string(path)?;
    let tokens: OAuthTokens = serde_json::from_str(&json)?;
    Ok(tokens)
}

/// Elimina i token OAuth dal disco per un provider.
pub fn delete_tokens(provider: ConnectorProvider) -> Result<(), ConnectorError> {
    let dir = tokens_dir();
    let filename = match provider {
        ConnectorProvider::Google => "google.json",
        ConnectorProvider::Microsoft => "microsoft.json",
    };

    let path = dir.join(filename);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connector_provider_serde() {
        let google = ConnectorProvider::Google;
        let json = serde_json::to_string(&google).unwrap();
        assert_eq!(json, "\"google\"");

        let ms: ConnectorProvider = serde_json::from_str("\"microsoft\"").unwrap();
        assert_eq!(ms, ConnectorProvider::Microsoft);
    }

    #[test]
    fn test_oauth_tokens_expiry() {
        let tokens = OAuthTokens {
            access_token: "test".into(),
            refresh_token: "test".into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
        };
        assert!(!tokens.is_expired());

        let expired = OAuthTokens {
            access_token: "test".into(),
            refresh_token: "test".into(),
            expires_at: Utc::now() - chrono::Duration::hours(1),
        };
        assert!(expired.is_expired());
    }

    #[test]
    fn test_email_serde() {
        let email = Email {
            id: "msg123".into(),
            from: "test@example.com".into(),
            to: vec!["me@example.com".into()],
            subject: "Test email".into(),
            body_preview: "Ciao, questo è un test.".into(),
            received_at: Utc::now(),
            unread: true,
            has_attachments: false,
        };
        let json = serde_json::to_string(&email).unwrap();
        let decoded: Email = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "msg123");
        assert!(decoded.unread);
    }

    #[test]
    fn test_cal_event_serde() {
        let event = CalEvent {
            id: "evt456".into(),
            title: "Riunione".into(),
            start: Utc::now(),
            end: Utc::now() + chrono::Duration::hours(1),
            location: Some("Ufficio".into()),
            description: None,
            all_day: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: CalEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.title, "Riunione");
    }
}
