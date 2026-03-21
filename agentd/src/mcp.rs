//! Client MCP (Model Context Protocol) — connessione a server esterni di tool.
//!
//! Ogni server MCP è un endpoint HTTP che espone tool via JSON-RPC 2.0.
//! Il client si connette ai server configurati, elenca i tool disponibili,
//! e li rende disponibili all'agente come tool normali.

use std::collections::HashMap;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, debug};

use agentos_common::config::McpConfig;

/// Descrizione di un tool esposto da un server MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    /// Nome del tool (es. "read_file", "search")
    pub name: String,
    /// Descrizione leggibile del tool
    #[serde(default)]
    pub description: String,
    /// Schema JSON dei parametri (opzionale)
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<serde_json::Value>,
}

/// Informazioni su un server MCP connesso.
#[derive(Debug, Clone)]
struct McpServer {
    /// Nome identificativo
    name: String,
    /// URL dell'endpoint
    url: String,
    /// Header per autenticazione
    headers: HashMap<String, String>,
    /// Tool disponibili su questo server
    tools: Vec<McpToolDef>,
}

/// Client MCP — gestisce le connessioni ai server e le chiamate ai tool.
pub struct McpClient {
    /// Server connessi, indicizzati per nome
    servers: Vec<McpServer>,
    /// Mappa tool_name → indice del server che lo espone
    tool_index: HashMap<String, usize>,
    /// Client HTTP condiviso
    http: reqwest::Client,
}

/// Richiesta JSON-RPC 2.0.
#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    id: u64,
}

/// Risposta JSON-RPC 2.0.
#[derive(Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
    #[allow(dead_code)]
    id: Option<serde_json::Value>,
}

/// Errore JSON-RPC 2.0.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

impl McpClient {
    /// Crea un nuovo client MCP e si connette a tutti i server configurati.
    /// I server non raggiungibili vengono ignorati con un warning.
    pub async fn new(config: &McpConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let mut servers = Vec::new();
        let mut tool_index = HashMap::new();

        for server_config in &config.servers {
            info!(
                name = %server_config.name,
                url = %server_config.url,
                "Connessione al server MCP"
            );

            // Prova a ottenere la lista dei tool dal server
            match Self::fetch_tools(&http, &server_config.url, &server_config.headers).await {
                Ok(tools) => {
                    let server_idx = servers.len();
                    let tool_count = tools.len();

                    // Registra ogni tool nell'indice globale
                    for tool in &tools {
                        // Prefissa con il nome del server per evitare conflitti
                        let qualified_name = format!("mcp_{}_{}", server_config.name, tool.name);
                        tool_index.insert(qualified_name, server_idx);
                        // Registra anche senza prefisso se non c'è conflitto
                        tool_index.entry(tool.name.clone()).or_insert(server_idx);
                    }

                    servers.push(McpServer {
                        name: server_config.name.clone(),
                        url: server_config.url.clone(),
                        headers: server_config.headers.clone(),
                        tools,
                    });

                    info!(
                        name = %server_config.name,
                        tool_count = tool_count,
                        "Server MCP connesso"
                    );
                }
                Err(e) => {
                    warn!(
                        name = %server_config.name,
                        error = %e,
                        "Impossibile connettersi al server MCP — ignorato"
                    );
                }
            }
        }

        Self { servers, tool_index, http }
    }

    /// Elenca i tool dal server MCP tramite `tools/list`.
    async fn fetch_tools(
        http: &reqwest::Client,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Vec<McpToolDef>> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/list".to_string(),
            params: None,
            id: 1,
        };

        let mut req_builder = http.post(url).json(&request);
        for (key, value) in headers {
            req_builder = req_builder.header(key, value);
        }

        let response = req_builder.send().await
            .map_err(|e| anyhow!("Errore connessione MCP: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Server MCP ha risposto con status {}", response.status()
            ));
        }

        let rpc_response: JsonRpcResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta MCP: {}", e))?;

        if let Some(err) = rpc_response.error {
            return Err(anyhow!("Errore MCP: {}", err.message));
        }

        let result = rpc_response.result
            .ok_or_else(|| anyhow!("Risposta MCP senza risultato"))?;

        // Il risultato di tools/list è { "tools": [...] }
        let tools: Vec<McpToolDef> = if let Some(tools_arr) = result.get("tools") {
            serde_json::from_value(tools_arr.clone())
                .map_err(|e| anyhow!("Errore parsing tool MCP: {}", e))?
        } else {
            // Alcuni server restituiscono direttamente l'array
            serde_json::from_value(result)
                .map_err(|e| anyhow!("Errore parsing tool MCP: {}", e))?
        };

        Ok(tools)
    }

    /// Verifica se un tool è gestito da un server MCP.
    pub fn is_mcp_tool(&self, tool_name: &str) -> bool {
        self.tool_index.contains_key(tool_name)
    }

    /// Chiama un tool MCP tramite `tools/call`.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let server_idx = self.tool_index.get(tool_name)
            .ok_or_else(|| anyhow!("Tool MCP '{}' non trovato", tool_name))?;

        let server = &self.servers[*server_idx];

        // Risolvi il nome del tool originale (senza prefisso mcp_)
        let original_name = if tool_name.starts_with(&format!("mcp_{}_", server.name)) {
            tool_name.strip_prefix(&format!("mcp_{}_", server.name)).unwrap_or(tool_name)
        } else {
            tool_name
        };

        debug!(
            tool = tool_name,
            server = %server.name,
            "Chiamata tool MCP"
        );

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": original_name,
                "arguments": args,
            })),
            id: 2,
        };

        let mut req_builder = self.http.post(&server.url).json(&request);
        for (key, value) in &server.headers {
            req_builder = req_builder.header(key, value);
        }

        let response = req_builder.send().await
            .map_err(|e| anyhow!("Errore connessione MCP {}: {}", server.name, e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Server MCP {} ha risposto con status {}",
                server.name,
                response.status()
            ));
        }

        let rpc_response: JsonRpcResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta MCP: {}", e))?;

        if let Some(err) = rpc_response.error {
            return Err(anyhow!("Errore MCP tool '{}': {}", tool_name, err.message));
        }

        rpc_response.result
            .ok_or_else(|| anyhow!("Risposta MCP senza risultato per '{}'", tool_name))
    }

    /// Genera la descrizione dei tool MCP per il system prompt dell'LLM.
    /// Formato compatibile con TOOLS_DESCRIPTION esistente.
    pub fn tools_description(&self) -> String {
        if self.servers.is_empty() {
            return String::new();
        }

        let mut desc = String::from("\nTOOL MCP (da server esterni):\n");

        for server in &self.servers {
            if server.tools.is_empty() {
                continue;
            }
            desc.push_str(&format!("\n[Server: {}]\n", server.name));
            for tool in &server.tools {
                // Mostra il nome con prefisso del server
                let qualified = format!("mcp_{}_{}", server.name, tool.name);
                desc.push_str(&format!("- {}: ", qualified));

                if !tool.description.is_empty() {
                    desc.push_str(&tool.description);
                } else {
                    desc.push_str("(nessuna descrizione)");
                }

                // Mostra i parametri dallo schema se disponibili
                if let Some(schema) = &tool.input_schema {
                    if let Some(props) = schema.get("properties") {
                        if let Some(obj) = props.as_object() {
                            let param_names: Vec<&String> = obj.keys().collect();
                            if !param_names.is_empty() {
                                desc.push_str(&format!(
                                    " — parametri: {{{}}}",
                                    param_names.iter()
                                        .map(|n| format!("\"{}\": ...", n))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ));
                            }
                        }
                    }
                }

                desc.push('\n');
            }
        }

        desc.push_str("\nPer chiamare un tool MCP, usa il nome completo (mcp_<server>_<tool>) come tool.\n");
        desc
    }

    /// Restituisce il numero totale di tool MCP disponibili.
    pub fn tool_count(&self) -> usize {
        self.servers.iter().map(|s| s.tools.len()).sum()
    }

    /// Restituisce i nomi di tutti i server connessi.
    pub fn server_names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_common::config::McpConfig;

    #[tokio::test]
    async fn test_empty_config() {
        let config = McpConfig { servers: vec![] };
        let client = McpClient::new(&config).await;
        assert_eq!(client.tool_count(), 0);
        assert!(client.tools_description().is_empty());
        assert!(!client.is_mcp_tool("anything"));
    }

    #[test]
    fn test_tool_description_format() {
        // Verifica che la descrizione generata sia ben formata
        let client = McpClient {
            servers: vec![McpServer {
                name: "test".to_string(),
                url: "http://localhost:9999".to_string(),
                headers: HashMap::new(),
                tools: vec![
                    McpToolDef {
                        name: "greet".to_string(),
                        description: "Saluta l'utente".to_string(),
                        input_schema: Some(serde_json::json!({
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"}
                            }
                        })),
                    },
                ],
            }],
            tool_index: {
                let mut m = HashMap::new();
                m.insert("mcp_test_greet".to_string(), 0);
                m.insert("greet".to_string(), 0);
                m
            },
            http: reqwest::Client::new(),
        };

        let desc = client.tools_description();
        assert!(desc.contains("mcp_test_greet"));
        assert!(desc.contains("Saluta l'utente"));
        assert!(desc.contains("\"name\""));
    }

    #[test]
    fn test_is_mcp_tool() {
        let client = McpClient {
            servers: vec![],
            tool_index: {
                let mut m = HashMap::new();
                m.insert("mcp_github_search".to_string(), 0);
                m
            },
            http: reqwest::Client::new(),
        };

        assert!(client.is_mcp_tool("mcp_github_search"));
        assert!(!client.is_mcp_tool("run_command"));
    }
}
