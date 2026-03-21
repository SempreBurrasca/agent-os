//! Flusso OAuth2 zero-config — l'utente dice "connetti gmail" e l'agente fa tutto.
//!
//! Architettura:
//! 1. Avvia un mini server HTTP su localhost (porta random)
//! 2. Apre il browser con l'URL OAuth
//! 3. Riceve il callback con il codice di autorizzazione
//! 4. Scambia il codice per access_token + refresh_token
//! 5. Salva i token in ~/.agentos/tokens/{provider}.json
//!
//! Provider supportati: Google (Gmail + Calendar), Microsoft (Outlook + Calendar)

use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::connectors::{ConnectorProvider, OAuthTokens, save_tokens};

// ============================================================
// Costanti OAuth
// ============================================================

/// URL autorizzazione Google
const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// URL token Google
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Scopes Google per email e calendario
const GOOGLE_SCOPES: &str = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/calendar.events";

/// URL autorizzazione Microsoft
const MS_AUTH_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2/authorize";
/// URL token Microsoft
const MS_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2/token";
/// Scopes Microsoft per email e calendario
const MS_SCOPES: &str = "Mail.Read Calendars.ReadWrite offline_access";

/// Placeholder client_id per Google (sovrascritto da env GOOGLE_CLIENT_ID)
const GOOGLE_PLACEHOLDER_CLIENT_ID: &str = "PLACEHOLDER_GOOGLE_CLIENT_ID";
/// Placeholder client_secret per Google (sovrascritto da env GOOGLE_CLIENT_SECRET)
const GOOGLE_PLACEHOLDER_CLIENT_SECRET: &str = "PLACEHOLDER_GOOGLE_CLIENT_SECRET";

/// Placeholder client_id per Microsoft (sovrascritto da env MICROSOFT_CLIENT_ID)
const MS_PLACEHOLDER_CLIENT_ID: &str = "PLACEHOLDER_MICROSOFT_CLIENT_ID";

/// Timeout per la callback OAuth (5 minuti)
const CALLBACK_TIMEOUT_SECS: u64 = 300;

// ============================================================
// Struttura principale
// ============================================================

/// Gestisce il flusso OAuth2 completo via browser redirect su localhost.
pub struct OAuthFlow {
    http: Client,
}

/// Configurazione specifica per ogni provider OAuth.
struct ProviderConfig {
    /// Nome del provider per log e messaggi
    name: String,
    /// URL di autorizzazione
    auth_url: String,
    /// URL per lo scambio del codice
    token_url: String,
    /// Scopes richiesti
    scopes: String,
    /// Client ID
    client_id: String,
    /// Client secret (vuoto per PKCE/public clients)
    client_secret: String,
    /// Tipo di connettore per il salvataggio token
    connector_provider: ConnectorProvider,
}

/// Risposta dello scambio codice → token (Google).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    #[allow(dead_code)]
    token_type: Option<String>,
}

impl OAuthFlow {
    /// Crea un nuovo OAuthFlow.
    pub fn new() -> Self {
        Self {
            http: Client::new(),
        }
    }

    /// Avvia il flusso OAuth completo per il provider specificato.
    ///
    /// 1. Avvia un server HTTP locale su una porta random
    /// 2. Costruisce l'URL OAuth con redirect_uri = http://localhost:{porta}/callback
    /// 3. Apre il browser
    /// 4. Attende il callback (max 5 minuti)
    /// 5. Scambia il codice per i token
    /// 6. Salva i token in ~/.agentos/tokens/
    pub async fn start_flow(&self, provider: &str) -> Result<OAuthTokens> {
        let config = self.get_provider_config(provider)?;

        info!(provider = %config.name, "Avvio flusso OAuth");

        // 1. Avvia il listener TCP su una porta random
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
            .map_err(|e| anyhow!("Impossibile avviare il server locale: {}", e))?;
        let local_addr = listener.local_addr()?;
        let port = local_addr.port();
        let redirect_uri = format!("http://localhost:{}/callback", port);

        info!(port = port, "Server OAuth locale avviato su {}", redirect_uri);

        // 2. Genera state random per CSRF protection
        let state = uuid::Uuid::new_v4().to_string();

        // 3. Costruisci l'URL OAuth
        let auth_url = format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
            config.auth_url,
            urlencoding(&config.client_id),
            urlencoding(&redirect_uri),
            urlencoding(&config.scopes),
            urlencoding(&state),
        );

        // 4. Apri il browser
        info!("Apertura browser per autorizzazione {}", config.name);
        open_browser(&auth_url);

        // 5. Attendi il callback con timeout
        let code = self.wait_for_callback(listener, &state).await?;

        info!("Codice di autorizzazione ricevuto, scambio per token...");

        // 6. Scambia il codice per i token
        let tokens = self.exchange_code(&config, &code, &redirect_uri).await?;

        // 7. Salva i token su disco
        save_tokens(config.connector_provider, &tokens)
            .map_err(|e| anyhow!("Errore salvataggio token: {}", e))?;

        info!(provider = %config.name, "Autenticazione completata e token salvati");

        Ok(tokens)
    }

    /// Restituisce la configurazione OAuth per il provider specificato.
    fn get_provider_config(&self, provider: &str) -> Result<ProviderConfig> {
        match provider.to_lowercase().as_str() {
            "gmail" | "google" => {
                let client_id = std::env::var("GOOGLE_CLIENT_ID")
                    .unwrap_or_else(|_| GOOGLE_PLACEHOLDER_CLIENT_ID.to_string());
                let client_secret = std::env::var("GOOGLE_CLIENT_SECRET")
                    .unwrap_or_else(|_| GOOGLE_PLACEHOLDER_CLIENT_SECRET.to_string());

                if client_id == GOOGLE_PLACEHOLDER_CLIENT_ID {
                    warn!("Client ID Google non configurato. Imposta la variabile GOOGLE_CLIENT_ID.");
                }

                Ok(ProviderConfig {
                    name: "Google".to_string(),
                    auth_url: GOOGLE_AUTH_URL.to_string(),
                    token_url: GOOGLE_TOKEN_URL.to_string(),
                    scopes: GOOGLE_SCOPES.to_string(),
                    client_id,
                    client_secret,
                    connector_provider: ConnectorProvider::Google,
                })
            }
            "outlook" | "microsoft" => {
                let client_id = std::env::var("MICROSOFT_CLIENT_ID")
                    .unwrap_or_else(|_| MS_PLACEHOLDER_CLIENT_ID.to_string());

                if client_id == MS_PLACEHOLDER_CLIENT_ID {
                    warn!("Client ID Microsoft non configurato. Imposta la variabile MICROSOFT_CLIENT_ID.");
                }

                Ok(ProviderConfig {
                    name: "Microsoft".to_string(),
                    auth_url: MS_AUTH_URL.to_string(),
                    token_url: MS_TOKEN_URL.to_string(),
                    scopes: MS_SCOPES.to_string(),
                    client_id,
                    client_secret: String::new(), // Microsoft usa PKCE, non serve client_secret
                    connector_provider: ConnectorProvider::Microsoft,
                })
            }
            other => Err(anyhow!(
                "Provider '{}' non supportato. Usa: gmail, google, outlook, microsoft",
                other
            )),
        }
    }

    /// Attende la callback OAuth sul listener TCP.
    /// Parsa la query string per estrarre `code` e `state`.
    /// Risponde con una pagina HTML di conferma.
    async fn wait_for_callback(
        &self,
        listener: tokio::net::TcpListener,
        expected_state: &str,
    ) -> Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let timeout = std::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS);

        // Attendi una connessione con timeout
        let (mut stream, _addr) = tokio::time::timeout(timeout, listener.accept())
            .await
            .map_err(|_| anyhow!("Timeout: nessuna risposta dal browser entro {} minuti. Riprova.", CALLBACK_TIMEOUT_SECS / 60))?
            .map_err(|e| anyhow!("Errore accettazione connessione: {}", e))?;

        // Leggi la richiesta HTTP
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        debug!("Ricevuta richiesta callback OAuth");

        // Parsa la query string dalla richiesta GET
        // Formato: GET /callback?code=XXX&state=YYY HTTP/1.1
        let (code, error) = parse_callback_request(&request);

        // Verifica errori
        if let Some(err) = error {
            // Rispondi con pagina di errore
            let error_html = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
                 <!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>AgentOS</title>\
                 <style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#1a1a2e;color:#e0e0e0}}\
                 .box{{text-align:center;padding:2rem;border-radius:12px;background:#16213e}}</style></head>\
                 <body><div class=\"box\"><h1>Errore</h1><p>{}</p><p>Chiudi questa finestra e riprova.</p></div></body></html>",
                err
            );
            stream.write_all(error_html.as_bytes()).await.ok();
            stream.shutdown().await.ok();
            return Err(anyhow!("Errore OAuth: {}", err));
        }

        let code = code.ok_or_else(|| anyhow!("Codice di autorizzazione non trovato nella callback"))?;

        // Verifica lo state (CSRF protection)
        if let Some(state) = parse_query_param(&request, "state") {
            if state != expected_state {
                let error_html = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
                     <!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>AgentOS</title>\
                     <style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#1a1a2e;color:#e0e0e0}}\
                     .box{{text-align:center;padding:2rem;border-radius:12px;background:#16213e}}</style></head>\
                     <body><div class=\"box\"><h1>Errore</h1><p>State non valido (possibile attacco CSRF). Riprova.</p></div></body></html>"
                );
                stream.write_all(error_html.as_bytes()).await.ok();
                stream.shutdown().await.ok();
                return Err(anyhow!("State OAuth non corrisponde — possibile CSRF"));
            }
        }

        // Rispondi con pagina di successo
        let success_html = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
             <!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>AgentOS</title>\
             <style>body{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#1a1a2e;color:#e0e0e0}\
             .box{text-align:center;padding:2rem;border-radius:12px;background:#16213e}\
             .check{font-size:3rem;color:#00e676}</style></head>\
             <body><div class=\"box\"><div class=\"check\">&#10003;</div><h1>Connesso!</h1>\
             <p>Puoi chiudere questa finestra e tornare al terminale.</p></div></body></html>";

        stream.write_all(success_html.as_bytes()).await.ok();
        stream.shutdown().await.ok();

        Ok(code)
    }

    /// Scambia il codice di autorizzazione per access_token + refresh_token.
    async fn exchange_code(
        &self,
        config: &ProviderConfig,
        code: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokens> {
        let mut params = vec![
            ("code", code.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            ("grant_type", "authorization_code".to_string()),
            ("client_id", config.client_id.clone()),
        ];

        // Google richiede il client_secret, Microsoft no (PKCE)
        if !config.client_secret.is_empty() {
            params.push(("client_secret", config.client_secret.clone()));
        }

        let resp = self.http
            .post(&config.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| anyhow!("Errore scambio codice: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Errore scambio token ({}): {}",
                status, body
            ));
        }

        let token_resp: TokenResponse = resp.json().await
            .map_err(|e| anyhow!("Errore parsing risposta token: {}", e))?;

        let tokens = OAuthTokens {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token.unwrap_or_default(),
            expires_at: Utc::now() + Duration::seconds(token_resp.expires_in as i64),
        };

        Ok(tokens)
    }
}

// ============================================================
// Helper
// ============================================================

/// Apre un URL nel browser predefinito del sistema.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd").args(["/c", "start", url]).spawn();
    }
}

/// Parsa la richiesta HTTP callback ed estrae il codice e l'eventuale errore.
/// Restituisce (Option<code>, Option<error>).
fn parse_callback_request(request: &str) -> (Option<String>, Option<String>) {
    // Estrai la prima riga della richiesta: GET /callback?code=XXX&state=YYY HTTP/1.1
    let first_line = request.lines().next().unwrap_or("");

    // Controlla se c'è un errore (es. access_denied)
    if let Some(error) = parse_query_param_from_line(first_line, "error") {
        let description = parse_query_param_from_line(first_line, "error_description")
            .unwrap_or_else(|| error.clone());
        return (None, Some(url_decode_simple(&description)));
    }

    // Estrai il codice di autorizzazione
    let code = parse_query_param_from_line(first_line, "code");
    (code, None)
}

/// Estrae un parametro dalla query string della prima riga di una richiesta HTTP.
fn parse_query_param(request: &str, param: &str) -> Option<String> {
    let first_line = request.lines().next()?;
    parse_query_param_from_line(first_line, param)
}

/// Estrae un parametro dalla query string di una riga di richiesta HTTP.
fn parse_query_param_from_line(line: &str, param: &str) -> Option<String> {
    // Formato: GET /callback?code=XXX&state=YYY HTTP/1.1
    let query_start = line.find('?')?;
    let query_end = line.find(" HTTP").unwrap_or(line.len());
    let query = &line[query_start + 1..query_end];

    let prefix = format!("{}=", param);
    for pair in query.split('&') {
        if pair.starts_with(&prefix) {
            return Some(pair[prefix.len()..].to_string());
        }
    }
    None
}

/// Decodifica URL encoding minimale.
fn url_decode_simple(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

/// Codifica URL semplice per i parametri.
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '&' => "%26".to_string(),
            '=' => "%3D".to_string(),
            '+' => "%2B".to_string(),
            '#' => "%23".to_string(),
            '/' => "%2F".to_string(),
            ':' => "%3A".to_string(),
            '@' => "%40".to_string(),
            _ if c.is_ascii_alphanumeric() || "-._~".contains(c) => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

// ============================================================
// Test
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_callback_success() {
        let request = "GET /callback?code=4/0AX4XfWh&state=abc123 HTTP/1.1\r\nHost: localhost:9876\r\n";
        let (code, error) = parse_callback_request(request);
        assert_eq!(code, Some("4/0AX4XfWh".to_string()));
        assert!(error.is_none());
    }

    #[test]
    fn test_parse_callback_error() {
        let request = "GET /callback?error=access_denied&error_description=User+denied+access HTTP/1.1\r\n";
        let (code, error) = parse_callback_request(request);
        assert!(code.is_none());
        assert!(error.is_some());
        assert!(error.unwrap().contains("denied"));
    }

    #[test]
    fn test_parse_query_param() {
        let request = "GET /callback?code=ABC&state=XYZ HTTP/1.1\r\nHost: localhost\r\n";
        assert_eq!(parse_query_param(request, "code"), Some("ABC".to_string()));
        assert_eq!(parse_query_param(request, "state"), Some("XYZ".to_string()));
        assert_eq!(parse_query_param(request, "missing"), None);
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoding("https://example.com"), "https%3A%2F%2Fexample.com");
    }

    #[test]
    fn test_url_decode_simple() {
        assert_eq!(url_decode_simple("hello%20world"), "hello world");
        assert_eq!(url_decode_simple("User+denied+access"), "User denied access");
    }

    #[test]
    fn test_provider_config_google() {
        // Imposta env vars temporanei (non modifica lo stato globale per gli altri test)
        let flow = OAuthFlow::new();
        let config = flow.get_provider_config("gmail").unwrap();
        assert_eq!(config.name, "Google");
        assert_eq!(config.auth_url, GOOGLE_AUTH_URL);
        assert!(!config.scopes.is_empty());
    }

    #[test]
    fn test_provider_config_microsoft() {
        let flow = OAuthFlow::new();
        let config = flow.get_provider_config("outlook").unwrap();
        assert_eq!(config.name, "Microsoft");
        assert_eq!(config.auth_url, MS_AUTH_URL);
    }

    #[test]
    fn test_provider_config_aliases() {
        let flow = OAuthFlow::new();
        // Tutti gli alias devono funzionare
        assert!(flow.get_provider_config("gmail").is_ok());
        assert!(flow.get_provider_config("google").is_ok());
        assert!(flow.get_provider_config("outlook").is_ok());
        assert!(flow.get_provider_config("microsoft").is_ok());
        // Provider sconosciuto
        assert!(flow.get_provider_config("notion").is_err());
    }

    #[test]
    fn test_parse_callback_no_query() {
        let request = "GET /callback HTTP/1.1\r\n";
        let (code, error) = parse_callback_request(request);
        assert!(code.is_none());
        assert!(error.is_none());
    }

    #[test]
    fn test_parse_callback_multiple_params() {
        let request = "GET /callback?code=MYCODE&state=MYSTATE&scope=email HTTP/1.1\r\n";
        let (code, _) = parse_callback_request(request);
        assert_eq!(code, Some("MYCODE".to_string()));
        assert_eq!(
            parse_query_param(request, "scope"),
            Some("email".to_string())
        );
    }
}
