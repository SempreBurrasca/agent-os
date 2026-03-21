//! Connettore Microsoft — Outlook Mail e Calendar via Microsoft Graph API.
//!
//! Utilizza OAuth2 device code flow per l'autenticazione CLI.
//! Endpoint:
//!   - Microsoft Graph API v1.0: https://graph.microsoft.com/v1.0/
//!   - Device code: https://login.microsoftonline.com/{tenant}/oauth2/v2.0/devicecode
//!   - Token: https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token
//!
//! Scopes richiesti:
//!   - Mail.Read
//!   - Calendars.ReadWrite
//!   - offline_access (per il refresh token)

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, warn};

use super::{
    CalEvent, CalendarConnector, ConnectorError, ConnectorProvider,
    CreateEventParams, DeviceCodeResponse, Email, EmailConnector,
    OAuthTokens, load_tokens, save_tokens,
};

// ============================================================
// Costanti
// ============================================================

/// Base URL Microsoft Graph API v1.0.
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Scopes OAuth richiesti per email e calendario.
const SCOPES: &str = "Mail.Read Calendars.ReadWrite offline_access";

// ============================================================
// Client Microsoft
// ============================================================

/// Client per le API Microsoft Graph (Outlook Mail + Calendar).
/// Gestisce autenticazione, refresh token e chiamate API.
pub struct OutlookConnector {
    /// Client HTTP condiviso
    http: Client,
    /// Application (client) ID registrato su Azure AD
    client_id: String,
    /// Tenant ID (o "common" per multi-tenant)
    tenant_id: String,
    /// Token OAuth correnti (caricati da disco o appena ottenuti)
    tokens: tokio::sync::RwLock<Option<OAuthTokens>>,
}

impl OutlookConnector {
    /// Crea un nuovo connettore Microsoft con le credenziali OAuth.
    pub fn new(client_id: &str, tenant_id: &str) -> Self {
        // Prova a caricare i token salvati
        let saved_tokens = load_tokens(ConnectorProvider::Microsoft).ok();

        Self {
            http: Client::new(),
            client_id: client_id.to_string(),
            tenant_id: tenant_id.to_string(),
            tokens: tokio::sync::RwLock::new(saved_tokens),
        }
    }

    /// Crea un connettore con token già presenti (usato dopo il flusso OAuth).
    pub fn with_tokens(client_id: &str, tenant_id: &str, tokens: OAuthTokens) -> Self {
        Self {
            http: Client::new(),
            client_id: client_id.to_string(),
            tenant_id: tenant_id.to_string(),
            tokens: tokio::sync::RwLock::new(Some(tokens)),
        }
    }

    /// Verifica se il connettore ha token validi.
    pub async fn is_authenticated(&self) -> bool {
        let tokens = self.tokens.read().await;
        tokens.is_some()
    }

    /// URL per il device code endpoint (dipende dal tenant).
    fn device_code_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            self.tenant_id
        )
    }

    /// URL per il token endpoint (dipende dal tenant).
    fn token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        )
    }

    // ── OAuth2 Device Code Flow ──

    /// Passo 1: Richiedi un device code per l'autenticazione Microsoft.
    pub async fn start_device_flow(&self) -> Result<DeviceCodeResponse, ConnectorError> {
        debug!("Avvio device code flow Microsoft");

        let resp = self.http
            .post(&self.device_code_url())
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

        let data: MsDeviceCodeResp = resp.json().await?;

        Ok(DeviceCodeResponse {
            device_code: data.device_code,
            user_code: data.user_code,
            verification_url: data.verification_uri,
            interval: data.interval,
            expires_in: data.expires_in,
        })
    }

    /// Passo 2: Polling per ottenere i token dopo che l'utente ha autorizzato.
    pub async fn poll_for_token(&self, device_code: &str) -> Result<OAuthTokens, ConnectorError> {
        debug!("Polling token Microsoft");

        let resp = self.http
            .post(&self.token_url())
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            // Microsoft restituisce "authorization_pending" se l'utente non ha ancora autorizzato
            if body.contains("authorization_pending") {
                return Err(ConnectorError::OAuth("authorization_pending".into()));
            }
            if body.contains("slow_down") {
                return Err(ConnectorError::OAuth("slow_down".into()));
            }
            if body.contains("expired_token") {
                return Err(ConnectorError::OAuth("Codice scaduto. Riprova /connect outlook".into()));
            }
            return Err(ConnectorError::OAuth(body));
        }

        let data: MsTokenResp = resp.json().await?;
        let tokens = OAuthTokens {
            access_token: data.access_token,
            refresh_token: data.refresh_token.unwrap_or_default(),
            expires_at: Utc::now() + Duration::seconds(data.expires_in as i64),
        };

        // Salva i token su disco
        save_tokens(ConnectorProvider::Microsoft, &tokens)?;

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

        debug!("Refresh access token Microsoft");

        let resp = self.http
            .post(&self.token_url())
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("refresh_token", &refresh_token),
                ("grant_type", "refresh_token"),
                ("scope", SCOPES),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(body = %body, "Errore refresh token Microsoft");
            return Err(ConnectorError::OAuth(format!("Refresh fallito: {}", body)));
        }

        let data: MsTokenResp = resp.json().await?;
        let new_tokens = OAuthTokens {
            access_token: data.access_token,
            refresh_token: data.refresh_token.unwrap_or(refresh_token),
            expires_at: Utc::now() + Duration::seconds(data.expires_in as i64),
        };

        // Salva su disco e in memoria
        save_tokens(ConnectorProvider::Microsoft, &new_tokens)?;
        let mut lock = self.tokens.write().await;
        *lock = Some(new_tokens);

        Ok(())
    }

    /// Ottieni un access token valido (rinnova se necessario).
    async fn get_access_token(&self) -> Result<String, ConnectorError> {
        {
            let tokens = self.tokens.read().await;
            match tokens.as_ref() {
                Some(t) if !t.is_expired() => return Ok(t.access_token.clone()),
                Some(_) => {} // scaduto, serve refresh
                None => return Err(ConnectorError::NotConfigured(
                    "Microsoft non autenticato. Usa /connect outlook".into()
                )),
            }
        }

        self.refresh_access_token().await?;

        let tokens = self.tokens.read().await;
        Ok(tokens.as_ref().unwrap().access_token.clone())
    }

    /// Esegue una richiesta GET autenticata verso Microsoft Graph.
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

    /// Esegue una richiesta POST autenticata verso Microsoft Graph.
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
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::Api { status, message: body_text });
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }
}

// ============================================================
// Implementazione EmailConnector per Microsoft (Outlook)
// ============================================================

#[async_trait]
impl EmailConnector for OutlookConnector {
    /// Elenca le email recenti dalla casella Outlook.
    async fn list_emails(&self, max_results: u32) -> Result<Vec<Email>, ConnectorError> {
        debug!(max = max_results, "Outlook: lista email recenti");

        let url = format!(
            "{}/me/messages?$top={}&$orderby=receivedDateTime%20desc&$select=id,subject,from,toRecipients,bodyPreview,receivedDateTime,isRead,hasAttachments",
            GRAPH_BASE, max_results
        );

        let resp = self.api_get(&url).await?;

        let emails: Vec<Email> = resp
            .get("value")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(parse_outlook_message).collect())
            .unwrap_or_default();

        Ok(emails)
    }

    /// Legge una singola email per ID.
    async fn read_email(&self, email_id: &str) -> Result<Email, ConnectorError> {
        debug!(id = email_id, "Outlook: lettura email");

        let url = format!(
            "{}/me/messages/{}?$select=id,subject,from,toRecipients,bodyPreview,body,receivedDateTime,isRead,hasAttachments",
            GRAPH_BASE, email_id
        );

        let msg = self.api_get(&url).await?;
        Ok(parse_outlook_message(&msg))
    }

    /// Cerca email con la sintassi di ricerca Microsoft Graph.
    async fn search_emails(&self, query: &str, max_results: u32) -> Result<Vec<Email>, ConnectorError> {
        debug!(query = query, max = max_results, "Outlook: ricerca email");

        // Microsoft Graph usa $search per la ricerca full-text
        let url = format!(
            "{}/me/messages?$top={}&$search=\"{}\"&$select=id,subject,from,toRecipients,bodyPreview,receivedDateTime,isRead,hasAttachments",
            GRAPH_BASE,
            max_results,
            query.replace('"', "\\\""),
        );

        let resp = self.api_get(&url).await?;

        let emails: Vec<Email> = resp
            .get("value")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(parse_outlook_message).collect())
            .unwrap_or_default();

        Ok(emails)
    }
}

// ============================================================
// Implementazione CalendarConnector per Microsoft (Outlook Calendar)
// ============================================================

#[async_trait]
impl CalendarConnector for OutlookConnector {
    /// Elenca i prossimi eventi dal calendario principale.
    async fn list_events(&self, days: u32, max_results: u32) -> Result<Vec<CalEvent>, ConnectorError> {
        debug!(days = days, max = max_results, "Outlook Calendar: lista eventi");

        let now = Utc::now();
        let time_max = now + Duration::days(days as i64);

        // calendarView richiede startDateTime e endDateTime
        let url = format!(
            "{}/me/calendarView?startDateTime={}&endDateTime={}&$top={}&$orderby=start/dateTime&$select=id,subject,start,end,location,bodyPreview,isAllDay",
            GRAPH_BASE,
            now.to_rfc3339(),
            time_max.to_rfc3339(),
            max_results,
        );

        let resp = self.api_get(&url).await?;

        let events: Vec<CalEvent> = resp
            .get("value")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(parse_outlook_event).collect())
            .unwrap_or_default();

        Ok(events)
    }

    /// Crea un nuovo evento nel calendario principale.
    async fn create_event(&self, params: CreateEventParams) -> Result<CalEvent, ConnectorError> {
        debug!(title = %params.title, "Outlook Calendar: creazione evento");

        let body = serde_json::json!({
            "subject": params.title,
            "start": {
                "dateTime": params.start.format("%Y-%m-%dT%H:%M:%S").to_string(),
                "timeZone": "UTC",
            },
            "end": {
                "dateTime": params.end.format("%Y-%m-%dT%H:%M:%S").to_string(),
                "timeZone": "UTC",
            },
            "location": {
                "displayName": params.location.unwrap_or_default(),
            },
            "body": {
                "contentType": "text",
                "content": params.description.unwrap_or_default(),
            },
        });

        let url = format!("{}/me/events", GRAPH_BASE);
        let resp = self.api_post(&url, &body).await?;

        parse_outlook_event(&resp).ok_or_else(|| ConnectorError::Api {
            status: 0,
            message: "Impossibile parsare l'evento creato".into(),
        })
    }
}

// ============================================================
// Strutture di risposta Microsoft OAuth
// ============================================================

#[derive(Debug, Deserialize)]
struct MsDeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    expires_in: u64,
}

fn default_interval() -> u64 { 5 }

#[derive(Debug, Deserialize)]
struct MsTokenResp {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

// ============================================================
// Helper per parsing risposte API
// ============================================================

/// Parsa un messaggio Outlook (Microsoft Graph) in un `Email`.
fn parse_outlook_message(msg: &serde_json::Value) -> Email {
    let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let subject = msg.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let body_preview = msg.get("bodyPreview").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // "from" in Graph API ha struttura: { "emailAddress": { "name": "...", "address": "..." } }
    let from = msg
        .pointer("/from/emailAddress/address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // "toRecipients" è un array di { "emailAddress": { "address": "..." } }
    let to: Vec<String> = msg
        .get("toRecipients")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.pointer("/emailAddress/address").and_then(|a| a.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Data ricezione (ISO 8601)
    let received_at = msg
        .get("receivedDateTime")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    let unread = msg.get("isRead").and_then(|v| v.as_bool()).map(|r| !r).unwrap_or(false);
    let has_attachments = msg.get("hasAttachments").and_then(|v| v.as_bool()).unwrap_or(false);

    Email {
        id,
        from,
        to,
        subject,
        body_preview,
        received_at,
        unread,
        has_attachments,
    }
}

/// Parsa un evento Outlook Calendar (Microsoft Graph) in un `CalEvent`.
fn parse_outlook_event(event: &serde_json::Value) -> Option<CalEvent> {
    let id = event.get("id")?.as_str()?.to_string();
    let title = event.get("subject").and_then(|s| s.as_str()).unwrap_or("(senza titolo)").to_string();

    let all_day = event.get("isAllDay").and_then(|v| v.as_bool()).unwrap_or(false);

    // Microsoft Graph: start.dateTime e end.dateTime (senza timezone nel valore, timezone separata)
    let start_str = event.pointer("/start/dateTime")?.as_str()?;
    let end_str = event.pointer("/end/dateTime")?.as_str()?;

    // Prova parsing RFC3339 prima, poi formato senza timezone
    let start = parse_ms_datetime(start_str)?;
    let end = parse_ms_datetime(end_str)?;

    let location = event
        .pointer("/location/displayName")
        .and_then(|l| l.as_str())
        .filter(|l| !l.is_empty())
        .map(String::from);

    let description = event
        .get("bodyPreview")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
        .map(String::from);

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

/// Parsa una data/ora Microsoft Graph (può essere con o senza timezone).
fn parse_ms_datetime(s: &str) -> Option<DateTime<Utc>> {
    // Prova prima RFC3339 (con timezone)
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Microsoft Graph spesso restituisce "2026-03-20T10:00:00.0000000" senza timezone
    // Trattiamo come UTC
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc());
    }

    // Ultimo tentativo con millisecondi
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(naive.and_utc());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_outlook_message_basic() {
        let msg = serde_json::json!({
            "id": "AAMkAGI2",
            "subject": "Test email",
            "bodyPreview": "Questo è un test.",
            "from": {
                "emailAddress": {
                    "name": "Mario Rossi",
                    "address": "mario@example.com"
                }
            },
            "toRecipients": [{
                "emailAddress": {
                    "address": "me@example.com"
                }
            }],
            "receivedDateTime": "2026-03-20T10:00:00Z",
            "isRead": false,
            "hasAttachments": true,
        });

        let email = parse_outlook_message(&msg);
        assert_eq!(email.id, "AAMkAGI2");
        assert_eq!(email.from, "mario@example.com");
        assert_eq!(email.subject, "Test email");
        assert!(email.unread);
        assert!(email.has_attachments);
    }

    #[test]
    fn test_parse_outlook_event_basic() {
        let event = serde_json::json!({
            "id": "AAMkAGI2_evt",
            "subject": "Riunione",
            "start": {
                "dateTime": "2026-03-20T10:00:00.0000000",
                "timeZone": "UTC"
            },
            "end": {
                "dateTime": "2026-03-20T11:00:00.0000000",
                "timeZone": "UTC"
            },
            "location": {
                "displayName": "Sala B"
            },
            "bodyPreview": "Agenda della riunione",
            "isAllDay": false,
        });

        let cal = parse_outlook_event(&event).unwrap();
        assert_eq!(cal.id, "AAMkAGI2_evt");
        assert_eq!(cal.title, "Riunione");
        assert_eq!(cal.location, Some("Sala B".to_string()));
        assert!(!cal.all_day);
    }

    #[test]
    fn test_parse_ms_datetime() {
        // Con timezone
        let dt = parse_ms_datetime("2026-03-20T10:00:00Z").unwrap();
        assert_eq!(dt.hour(), 10);

        // Senza timezone (formato Microsoft)
        let dt2 = parse_ms_datetime("2026-03-20T10:00:00.0000000").unwrap();
        assert_eq!(dt2.hour(), 10);
    }

    use chrono::Timelike;
}
