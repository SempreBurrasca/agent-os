//! Connettore MCP — client Model Context Protocol.
//!
//! Connessione generica a servizi esterni che implementano MCP,
//! per recuperare e indicizzare dati da fonti esterne.
//! Implementazione completa nella Fase 4.

/// Configurazione del connettore MCP.
#[derive(Debug, Clone)]
pub struct McpConnectorConfig {
    pub server_url: String,
    pub auth_token: Option<String>,
}

/// Connettore MCP generico.
pub struct McpConnector {
    _config: McpConnectorConfig,
}

impl McpConnector {
    /// Crea un nuovo connettore MCP.
    pub fn new(config: McpConnectorConfig) -> Self {
        Self { _config: config }
    }

    // TODO Fase 4: implementare connect(), list_resources(), fetch()
}
