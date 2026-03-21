//! Connettore Email — connessione IMAP per indicizzare email.
//!
//! Scarica header e body delle email e li rende disponibili
//! come file virtuali per l'indicizzazione semantica.
//! Implementazione completa nella Fase 4.

/// Configurazione del connettore email.
#[derive(Debug, Clone)]
pub struct EmailConnectorConfig {
    pub imap_server: String,
    pub imap_port: u16,
    pub username: String,
    pub password: String,
    pub folders: Vec<String>,
}

/// Connettore email IMAP.
pub struct EmailConnector {
    _config: EmailConnectorConfig,
}

impl EmailConnector {
    /// Crea un nuovo connettore email.
    pub fn new(config: EmailConnectorConfig) -> Self {
        Self { _config: config }
    }

    // TODO Fase 4: implementare sync(), fetch_headers(), fetch_body()
}
