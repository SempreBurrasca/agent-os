//! Intent Engine — interpreta il linguaggio naturale dell'utente
//! e produce tool calls o risposte dirette.
//!
//! L'LLM riceve il system prompt con la lista dei tool disponibili
//! e risponde con JSON strutturato contenente explanation + tool_calls.

use anyhow::Result;
use tracing::{debug, warn};

use crate::llm::{LlmRouter, ChatMessage};
use crate::tools::{ToolCall, TOOLS_DESCRIPTION};

/// System prompt completo per l'agente.
const AGENT_SYSTEM_PROMPT: &str = r#"Sei l'agente AI di AgentOS. RISPONDI SOLO IN JSON VALIDO. Mai testo libero.
Lingua: italiano. Sei un agente OPERATIVO: esegui azioni, non spieghi e basta.
Se l'utente chiede qualcosa che richiede azione sul sistema, USA I TOOL.
Se è solo conversazione, rispondi con explanation e tool_calls vuoto [].
Per creare documenti/codice: scrivi in un file con run_command, poi apri con launch_app editor.
Sistema: Ubuntu 24.04, X11, directory /root/agent-os."#;

/// L'Intent Engine interpreta le richieste dell'utente.
pub struct IntentEngine {
    context: IntentContext,
}

/// Contesto fornito all'Intent Engine.
#[derive(Debug, Clone, Default)]
pub struct IntentContext {
    pub current_dir: String,
    pub dir_listing: String,
    pub recent_commands: Vec<String>,
    pub active_window: Option<(String, String)>,
}

/// Risposta strutturata dall'agente.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// Spiegazione per l'utente
    pub explanation: String,
    /// Tool da eseguire (può essere vuoto per risposte dirette)
    pub tool_calls: Vec<ToolCall>,
}

impl IntentEngine {
    pub fn new() -> Self {
        Self { context: IntentContext::default() }
    }

    pub fn update_context(&mut self, context: IntentContext) {
        self.context = context;
    }

    /// Interpreta una richiesta dell'utente con contesto multi-turno.
    /// `recent_interactions`: coppie (input_utente, risposta_agente) delle ultime interazioni.
    /// `patterns`: pattern appresi dalla memoria semantica da appendere al system prompt.
    pub async fn interpret(
        &self,
        user_input: &str,
        llm: &LlmRouter,
        recent_interactions: &[(String, String)],
        patterns: &[String],
    ) -> Result<AgentResponse> {
        self.interpret_with_mcp(user_input, llm, recent_interactions, patterns, "").await
    }

    /// Interpreta con supporto per tool MCP aggiuntivi.
    /// `mcp_tools_desc`: descrizione aggiuntiva dei tool MCP (può essere vuota).
    pub async fn interpret_with_mcp(
        &self,
        user_input: &str,
        llm: &LlmRouter,
        recent_interactions: &[(String, String)],
        patterns: &[String],
        mcp_tools_desc: &str,
    ) -> Result<AgentResponse> {
        let context_prompt = self.build_context_prompt();

        // Costruisci il system prompt con i pattern appresi e i tool MCP
        let mut system_content = format!("{}\n\n{}", AGENT_SYSTEM_PROMPT, TOOLS_DESCRIPTION);

        // Aggiungi tool MCP se presenti
        if !mcp_tools_desc.is_empty() {
            system_content.push_str(mcp_tools_desc);
        }

        // Aggiungi tool custom creati dall'agente
        let custom_desc = crate::tools::get_custom_tools_description();
        if !custom_desc.is_empty() {
            system_content.push_str(&custom_desc);
        }

        if !patterns.is_empty() {
            system_content.push_str("\n\nPATTERN APPRESI DALL'UTENTE:\n");
            for p in patterns {
                system_content.push_str(&format!("- {}\n", p));
            }
        }

        // Costruisci i messaggi multi-turno: system + ultime 5 interazioni + input corrente
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_content,
            },
        ];

        // Aggiungi le interazioni recenti come coppie user/assistant
        let last_n = if recent_interactions.len() > 5 { &recent_interactions[recent_interactions.len()-5..] } else { recent_interactions };
        for (user_msg, assistant_msg) in last_n {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: user_msg.clone(),
            });
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: assistant_msg.clone(),
            });
        }

        // Messaggio corrente dell'utente
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: format!("{}\n\nRichiesta utente: {}", context_prompt, user_input),
        });

        debug!(input = user_input, history_len = last_n.len(), "Interpretazione richiesta con contesto multi-turno");

        let response = llm.chat(&messages, 0.3).await?;
        self.parse_agent_response(&response.content)
    }

    /// Costruisce il prompt di contesto.
    fn build_context_prompt(&self) -> String {
        let mut parts = Vec::new();
        if !self.context.current_dir.is_empty() {
            parts.push(format!("Directory corrente: {}", self.context.current_dir));
        }
        if !self.context.dir_listing.is_empty() {
            parts.push(format!("File nella directory:\n{}", self.context.dir_listing));
        }
        if !self.context.recent_commands.is_empty() {
            let recent = self.context.recent_commands.iter()
                .take(5).cloned().collect::<Vec<_>>().join("\n");
            parts.push(format!("Ultimi comandi:\n{}", recent));
        }
        if parts.is_empty() { "Nessun contesto.".into() } else { parts.join("\n\n") }
    }

    /// Parsa la risposta JSON dall'LLM in AgentResponse.
    fn parse_agent_response(&self, response: &str) -> Result<AgentResponse> {
        let trimmed = response.trim();

        // Prova parsing diretto
        if let Some(resp) = self.try_parse_json(trimmed) {
            return Ok(resp);
        }

        // Estrai blocco JSON da markdown
        if let Some(json_block) = extract_json_block(trimmed) {
            if let Some(resp) = self.try_parse_json(&json_block) {
                return Ok(resp);
            }
        }

        // Cerca il primo { ... } nella risposta
        if let Some(start) = trimmed.find('{') {
            if let Some(end) = trimmed.rfind('}') {
                if let Some(resp) = self.try_parse_json(&trimmed[start..=end]) {
                    return Ok(resp);
                }
            }
        }

        // Fallback: tratta tutta la risposta come testo libero
        warn!("Risposta LLM non è JSON — uso come testo");
        Ok(AgentResponse {
            explanation: trimmed.to_string(),
            tool_calls: vec![],
        })
    }

    fn try_parse_json(&self, text: &str) -> Option<AgentResponse> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;

        let explanation = v.get("explanation")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls: Vec<ToolCall> = v.get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok())
            .unwrap_or_default();

        // Se ha il vecchio formato (commands), converti
        if tool_calls.is_empty() {
            if let Some(commands) = v.get("commands").and_then(|c| c.as_array()) {
                let calls: Vec<ToolCall> = commands.iter()
                    .filter_map(|c| c.as_str())
                    .map(|cmd| ToolCall {
                        tool: "run_command".to_string(),
                        args: serde_json::json!({"command": cmd}),
                    })
                    .collect();

                let expl = v.get("explanation")
                    .and_then(|e| e.as_str())
                    .unwrap_or(&explanation)
                    .to_string();

                if !calls.is_empty() {
                    return Some(AgentResponse { explanation: expl, tool_calls: calls });
                }
            }
        }

        Some(AgentResponse { explanation, tool_calls })
    }
}

fn extract_json_block(text: &str) -> Option<String> {
    for marker in &["```json", "```JSON", "```"] {
        if let Some(start) = text.find(marker) {
            let content_start = start + marker.len();
            if let Some(end) = text[content_start..].find("```") {
                return Some(text[content_start..content_start + end].trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_call_response() {
        let engine = IntentEngine::new();
        let json = r#"{
            "explanation": "Lancio Firefox per te.",
            "tool_calls": [
                {"tool": "launch_app", "args": {"app": "firefox"}}
            ]
        }"#;
        let resp = engine.parse_agent_response(json).unwrap();
        assert_eq!(resp.explanation, "Lancio Firefox per te.");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].tool, "launch_app");
    }

    #[test]
    fn test_parse_no_tools() {
        let engine = IntentEngine::new();
        let json = r#"{"explanation": "Sono le 14:30.", "tool_calls": []}"#;
        let resp = engine.parse_agent_response(json).unwrap();
        assert!(resp.tool_calls.is_empty());
        assert!(resp.explanation.contains("14:30"));
    }

    #[test]
    fn test_parse_old_commands_format() {
        let engine = IntentEngine::new();
        let json = r#"{"explanation": "Ecco i file.", "commands": ["ls -la"]}"#;
        let resp = engine.parse_agent_response(json).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].tool, "run_command");
    }

    #[test]
    fn test_parse_freetext_fallback() {
        let engine = IntentEngine::new();
        let resp = engine.parse_agent_response("Ciao, come stai?").unwrap();
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.explanation, "Ciao, come stai?");
    }

    #[test]
    fn test_parse_json_in_markdown() {
        let engine = IntentEngine::new();
        let text = "Ecco:\n```json\n{\"explanation\":\"OK\",\"tool_calls\":[]}\n```";
        let resp = engine.parse_agent_response(text).unwrap();
        assert_eq!(resp.explanation, "OK");
    }
}
