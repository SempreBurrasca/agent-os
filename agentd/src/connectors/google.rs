//! Connettore Google — Gmail API e Google Calendar API.
//!
//! Utilizza OAuth2 device code flow per l'autenticazione CLI.
//! Endpoint:
//!   - Gmail API v1: https://gmail.googleapis.com/gmail/v1/
//!   - Calendar API v3: https://www.googleapis.com/calendar/v3/
//!
//! Scopes richiesti:
//!   - https://www.googleapis.com/auth/gmail.readonly
//!   - https://www.googleapis.com/auth/calendar

use async_trait::async_trait;
use chrono::{DateTime, Utc, Duration};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{
    CalEvent, CalendarConnector, ConnectorError, ConnectorProvider,
    CreateEventParams, DeviceCodeResponse, Email, EmailConnector,
    OAuthTokens, load_tokens, save_tokens,
};

// ============================================================
// Costanti
// ============================================================

/// URL per il device code flow di Google.
const DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";
/// URL per lo scambio/refresh dei token.
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Base URL Gmail API v1.
const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
/// Base URL Calendar API v3.
const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Scopes OAuth richiesti per email e calendario.
const SCOPES: &str = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar";

// ============================================================
// Client Google
// ============================================================

/// Client per le API Google (Gmail + Calendar).
/// Gestisce autenticazione, refresh token e chiamate API.
pub struct GoogleConnector {
    /// Client HTTP condiviso
    http: Client,
    /// Client ID OAuth (tipo "desktop app")
    client_id: String,
    /// Client secret OAuth
    client_secret: String,
    /// Token OAuth correnti (caricati da disco o appena ottenuti)
    tokens: tokio::sync::RwLock<Option<OAuthTokens>>,
}

impl GoogleConnector {
    /// Crea un nuovo connettore Google con le credenziali OAuth.
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        // Prova a caricare i token salvati
        let saved_tokens = load_tokens(ConnectorProvider::Google).ok();

        Self {
            http: Client::new(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            tokens: tokio::sync::RwLock::new(saved_tokens),
        }
    }

    /// Crea un connettore con token già presenti (usato dopo il flusso OAuth).
    pub fn with_tokens(client_id: &str, client_secret: &str, tokens: OAuthTokens) -> Self {
        Self {
            http: Client::new(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            tokens: tokio::sync::RwLock::new(Some(tokens)),
        }
    }

    /// Verifica se il connettore ha token validi.
    pub async fn is_authenticated(&self) -> bool {
        let tokens = self.tokens.read().await;
        tokens.is_some()
    }

    // ── OAuth2 Device Code Flow ──

    /// Passo 1: Richiedi un device code per l'autenticazione.
    /// L'utente deve visitare l'URL e inserire il codice.
    pub async fn start_device_flow(&self) -> Result<DeviceCodeResponse, ConnectorError> {
        debug!("Avvio device code flow Google");

        let resp = self.http
            .post(DEVICE_CODE_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::Api {
                status,
                message: format!("Errore device code: {}", body),
            });
        }

        let data: GoogleDeviceCodeResp = resp.json().await?;

        Ok(DeviceCodeResponse {
            device_code: data.device_code,
            user_code: data.user_code,
            verification_url: data.verification_uri,
            interval: data.interval,
            expires_in: data.expires_in,
        })
    }

    /// Passo 2: Polling per ottenere i token dopo che l'utente ha autorizzato.
    /// Restituisce i token quando l'utente completa l'autorizzazione.
    pub async fn poll_for_token(&self, device_code: &str) -> Result<OAuthTokens, ConnectorError> {
        debug!("Polling token Google");

        let resp = self.http
            .post(TOKEN_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            // Google restituisce "authorization_pending" se l'utente non ha ancora autorizzato
            if body.contains("authorization_pending") {
                return Err(ConnectorError::OAuth("authorization_pending".into()));
            }
            if body.contains("slow_down") {
                return Err(ConnectorError::OAuth("slow_down".into()));
            }
            return Err(ConnectorError::OAuth(body));
        }

        let data: GoogleTokenResp = resp.json().await?;
        let tokens = OAuthTokens {
            access_token: data.access_token,
            refresh_token: data.refresh_token.unwrap_or_default(),
            expires_at: Utc::now() + Duration::seconds(data.expires_in as i64),
        };

        // Salva i token su disco
        save_tokens(ConnectorProvider::Google, &tokens)?;

        // Aggiorna i token in memoria
        let mut lock = self.tokens.write().await;
        *lock = Some(tokens.clone());

        Ok(tokens)
    }

    /// Rinnova l'access token usando il refresh token.
    async fn refresh_access_token(&self) -> Result<(), ConnectorError> {
        let refresh_token = {
            let tokens = self.tokens.read().await;
            match tokens.as_ref() {
                Some(t) => t.refresh_token.clone(),
                None => return Err(ConnectorError::TokenExpired),
            }
        };

        if refresh_token.is_empty() {
            return Err(ConnectorError::TokenExpired);
        }

        debug!("Refresh access token Google");

        let resp = self.http
            .post(TOKEN_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", &refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(body = %body, "Errore refresh token Google");
            return Err(ConnectorError::OAuth(format!("Refresh fallito: {}", body)));
        }

        let data: GoogleTokenResp = resp.json().await?;
        let new_tokens = OAuthTokens {
            access_token: data.access_token,
            refresh_token: data.refresh_token.unwrap_or(refresh_token),
            expires_at: Utc::now() + Duration::seconds(data.expires_in as i64),
        };

        // Salva su disco e in memoria
        save_tokens(ConnectorProvider::Google, &new_tokens)?;
        let mut lock = self.tokens.write().await;
        *lock = Some(new_tokens);

        Ok(())
    }

    /// Ottieni un access token valido (rinnova se necessario).
    async fn get_access_token(&self) -> Result<String, ConnectorError> {
        // Controlla se serve refresh
        {
            let tokens = self.tokens.read().await;
            match tokens.as_ref() {
                Some(t) if !t.is_expired() => return Ok(t.access_token.clone()),
                Some(_) => {} // scaduto, serve refresh
                None => return Err(ConnectorError::NotConfigured(
                    "Google non autenticato. Usa /connect google".into()
                )),
            }
        }

        // Rinnova il token
        self.refresh_access_token().await?;

        let tokens = self.tokens.read().await;
        Ok(tokens.as_ref().unwrap().access_token.clone())
    }

    /// Esegue una richiesta GET autenticata verso le API Google.
    async fn api_get(&self, url: &str) -> Result<serde_json::Value, ConnectorError> {
        let token = self.get_access_token().await?;

        let resp = self.http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::Api { status, message: body });
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }

    /// Esegue una richiesta POST autenticata verso le API Google.
    async fn api_post(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ConnectorError> {
        let token = self.get_access_token().await?;

        let resp = self.http
            .post(url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::Api { status, message: body });
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }
}

// ============================================================
// Implementazione EmailConnector per Google (Gmail API)
// ============================================================

#[async_trait]
impl EmailConnector for GoogleConnector {
    /// Elenca le email recenti dalla casella Gmail.
    /// Usa GET /users/me/messages con formato METADATA per efficienza.
    async fn list_emails(&self, max_results: u32) -> Result<Vec<Email>, ConnectorError> {
        debug!(max = max_results, "Gmail: lista email recenti");

        // Passo 1: ottieni gli ID dei messaggi
        let url = format!(
            "{}/users/me/messages?maxResults={}&labelIds=INBOX",
            GMAIL_BASE, max_results
        );
        let list_resp = self.api_get(&url).await?;

        let message_ids: Vec<String> = list_resp
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if message_ids.is_empty() {
            return Ok(vec![]);
        }

        // Passo 2: recupera i dettagli di ogni messaggio (formato METADATA)
        let mut emails = Vec::new();
        for msg_id in &message_ids {
            let detail_url = format!(
                "{}/users/me/messages/{}?format=metadata&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Subject&metadataHeaders=Date",
                GMAIL_BASE, msg_id
            );

            match self.api_get(&detail_url).await {
                Ok(msg) => {
                    let email = parse_gmail_message(&msg);
                    emails.push(email);
                }
                Err(e) => {
                    warn!(id = %msg_id, error = %e, "Errore lettura messaggio Gmail");
                }
            }
        }

        Ok(emails)
    }

    /// Legge una singola email per ID con corpo completo.
    async fn read_email(&self, email_id: &str) -> Result<Email, ConnectorError> {
        debug!(id = email_id, "Gmail: lettura email");

        let url = format!(
            "{}/users/me/messages/{}?format=full",
            GMAIL_BASE, email_id
        );
        let msg = self.api_get(&url).await?;
        Ok(parse_gmail_message(&msg))
    }

    /// Cerca email con la query Gmail (stessa sintassi della barra di ricerca).
    async fn search_emails(&self, query: &str, max_results: u32) -> Result<Vec<Email>, ConnectorError> {
        debug!(query = query, max = max_results, "Gmail: ricerca email");

        let url = format!(
            "{}/users/me/messages?maxResults={}&q={}",
            GMAIL_BASE,
            max_results,
            urlencoding_simple(query),
        );
        let list_resp = self.api_get(&url).await?;

        let message_ids: Vec<String> = list_resp
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let mut emails = Vec::new();
        for msg_id in &message_ids {
            let detail_url = format!(
                "{}/users/me/messages/{}?format=metadata&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Subject&metadataHeaders=Date",
                GMAIL_BASE, msg_id
            );
            if let Ok(msg) = self.api_get(&detail_url).await {
                emails.push(parse_gmail_message(&msg));
            }
        }

        Ok(emails)
    }
}

// ============================================================
// Implementazione CalendarConnector per Google (Calendar API)
// ============================================================

#[async_trait]
impl CalendarConnector for GoogleConnector {
    /// Elenca i prossimi eventi dal calendario principale.
    async fn list_events(&self, days: u32, max_results: u32) -> Result<Vec<CalEvent>, ConnectorError> {
        debug!(days = days, max = max_results, "Google Calendar: lista eventi");

        let now = Utc::now();
        let time_max = now + Duration::days(days as i64);

        let url = format!(
            "{}/calendars/primary/events?maxResults={}&timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
            CALENDAR_BASE,
            max_results,
            now.to_rfc3339(),
            time_max.to_rfc3339(),
        );

        let resp = self.api_get(&url).await?;

        let events: Vec<CalEvent> = resp
            .get("items")
            .and_then(|items| items.as_array())
            .map(|arr| arr.iter().filter_map(parse_gcal_event).collect())
            .unwrap_or_default();

        Ok(events)
    }

    /// Crea un nuovo evento nel calendario principale.
    async fn create_event(&self, params: CreateEventParams) -> Result<CalEvent, ConnectorError> {
        debug!(title = %params.title, "Google Calendar: creazione evento");

        let body = serde_json::json!({
            "summary": params.title,
            "start": {
                "dateTime": params.start.to_rfc3339(),
                "timeZone": "UTC",
            },
            "end": {
                "dateTime": params.end.to_rfc3339(),
                "timeZone": "UTC",
            },
            "location": params.location,
            "description": params.description,
        });

        let url = format!("{}/calendars/primary/events", CALENDAR_BASE);
        let resp = self.api_post(&url, &body).await?;

        parse_gcal_event(&resp).ok_or_else(|| ConnectorError::Api {
            status: 0,
            message: "Impossibile parsare l'evento creato".into(),
        })
    }
}

// ============================================================
// Strutture di risposta Google OAuth
// ============================================================

#[derive(Debug, Deserialize)]
struct GoogleDeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    expires_in: u64,
}

fn default_interval() -> u64 { 5 }

#[derive(Debug, Deserialize)]
struct GoogleTokenResp {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

// ============================================================
// Helper per parsing risposte API
// ============================================================

/// Parsa un messaggio Gmail in un `Email`.
fn parse_gmail_message(msg: &serde_json::Value) -> Email {
    let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let snippet = msg.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Estrai gli header dal payload
    let headers = msg
        .pointer("/payload/headers")
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();

    let get_header = |name: &str| -> String {
        headers.iter()
            .find(|h| h.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false))
            .and_then(|h| h.get("value").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    };

    let from = get_header("From");
    let to = get_header("To");
    let subject = get_header("Subject");
    let date_str = get_header("Date");

    // Parsa la data (best-effort)
    let received_at = chrono::DateTime::parse_from_rfc2822(&date_str)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| {
            // Prova con internalDate (millisecondi Unix)
            msg.get("internalDate")
                .and_then(|d| d.as_str())
                .and_then(|d| d.parse::<i64>().ok())
                .and_then(|ms| DateTime::from_timestamp(ms / 1000, 0))
                .unwrap_or_else(Utc::now)
        });

    // Controlla se è non letto (label UNREAD)
    let label_ids: Vec<String> = msg
        .get("labelIds")
        .and_then(|l| l.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let unread = label_ids.iter().any(|l| l == "UNREAD");

    // Controlla allegati
    let has_attachments = msg
        .pointer("/payload/parts")
        .and_then(|p| p.as_array())
        .map(|parts| parts.iter().any(|p| {
            p.get("filename").and_then(|f| f.as_str()).map(|f| !f.is_empty()).unwrap_or(false)
        }))
        .unwrap_or(false);

    Email {
        id,
        from,
        to: to.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        subject,
        body_preview: snippet,
        received_at,
        unread,
        has_attachments,
    }
}

/// Parsa un evento Google Calendar in un `CalEvent`.
fn parse_gcal_event(event: &serde_json::Value) -> Option<CalEvent> {
    let id = event.get("id")?.as_str()?.to_string();
    let title = event.get("summary").and_then(|s| s.as_str()).unwrap_or("(senza titolo)").to_string();

    // Google Calendar usa "dateTime" per eventi con orario, "date" per tutto il giorno
    let (start, all_day) = if let Some(dt) = event.pointer("/start/dateTime").and_then(|d| d.as_str()) {
        (DateTime::parse_from_rfc3339(dt).ok()?.with_timezone(&Utc), false)
    } else if let Some(date) = event.pointer("/start/date").and_then(|d| d.as_str()) {
        // Evento tutto il giorno — parsa come inizio giornata UTC
        let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        (naive.and_hms_opt(0, 0, 0)?.and_utc(), true)
    } else {
        return None;
    };

    let end = if let Some(dt) = event.pointer("/end/dateTime").and_then(|d| d.as_str()) {
        DateTime::parse_from_rfc3339(dt).ok()?.with_timezone(&Utc)
    } else if let Some(date) = event.pointer("/end/date").and_then(|d| d.as_str()) {
        let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        naive.and_hms_opt(23, 59, 59)?.and_utc()
    } else {
        start + Duration::hours(1)
    };

    let location = event.get("location").and_then(|l| l.as_str()).map(String::from);
    let description = event.get("description").and_then(|d| d.as_str()).map(String::from);

    Some(CalEvent {
        id,
        title,
        start,
        end,
        location,
        description,
        all_day,
    })
}

/// URL encoding minimale per le query.
fn urlencoding_simple(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '&' => "%26".to_string(),
            '=' => "%3D".to_string(),
            '+' => "%2B".to_string(),
            '#' => "%23".to_string(),
            _ if c.is_ascii_alphanumeric() || "-._~".contains(c) => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gmail_message_basic() {
        let msg = serde_json::json!({
            "id": "abc123",
            "snippet": "Ciao, come stai?",
            "labelIds": ["INBOX", "UNREAD"],
            "internalDate": "1700000000000",
            "payload": {
                "headers": [
                    {"name": "From", "value": "test@example.com"},
                    {"name": "To", "value": "me@example.com"},
                    {"name": "Subject", "value": "Oggetto test"},
                ]
            }
        });

        let email = parse_gmail_message(&msg);
        assert_eq!(email.id, "abc123");
        assert_eq!(email.from, "test@example.com");
        assert_eq!(email.subject, "Oggetto test");
        assert!(email.unread);
    }

    #[test]
    fn test_parse_gcal_event_datetime() {
        let event = serde_json::json!({
            "id": "evt1",
            "summary": "Riunione",
            "start": {"dateTime": "2026-03-20T10:00:00Z"},
            "end": {"dateTime": "2026-03-20T11:00:00Z"},
            "location": "Sala A",
        });

        let cal = parse_gcal_event(&event).unwrap();
        assert_eq!(cal.id, "evt1");
        assert_eq!(cal.title, "Riunione");
        assert_eq!(cal.location, Some("Sala A".to_string()));
        assert!(!cal.all_day);
    }

    #[test]
    fn test_parse_gcal_event_allday() {
        let event = serde_json::json!({
            "id": "evt2",
            "summary": "Vacanza",
            "start": {"date": "2026-03-20"},
            "end": {"date": "2026-03-21"},
        });

        let cal = parse_gcal_event(&event).unwrap();
        assert!(cal.all_day);
        assert_eq!(cal.title, "Vacanza");
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding_simple("hello world"), "hello%20world");
        assert_eq!(urlencoding_simple("a&b=c"), "a%26b%3Dc");
    }
}
