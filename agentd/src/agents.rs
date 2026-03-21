//! Sub-Agent System — l'agente può creare sub-agenti che lavorano in parallelo.
//!
//! Ogni sub-agente è un task tokio indipendente che:
//! - Riceve un prompt/obiettivo specifico
//! - Ha accesso a tutti i tool (run_command, create_tool, browse_url, ecc.)
//! - Esegue autonomamente il loop intent → tool → result
//! - Può fare più iterazioni (pensa → agisce → osserva → ripensa)
//! - Restituisce il risultato finale al parent
//!
//! Il parent può spawnare N agenti in parallelo e raccogliere i risultati.

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, debug, warn};

use crate::llm::{LlmRouter, ChatMessage};
use crate::tools::{self, ToolCall, ToolResult, TOOLS_DESCRIPTION};
use crate::mcp::McpClient;

/// Massimo numero di iterazioni per sub-agente (evita loop infiniti)
const MAX_ITERATIONS: usize = 10;
/// Massima profondità di nesting (agenti che spawnano agenti)
const MAX_DEPTH: usize = 3;

/// Stato di un sub-agente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    /// ID univoco del task
    pub id: String,
    /// Obiettivo/prompt assegnato
    pub objective: String,
    /// Stato corrente
    pub status: AgentStatus,
    /// Risultato finale (quando completato)
    pub result: Option<String>,
    /// Log delle azioni eseguite
    pub actions: Vec<AgentAction>,
    /// Profondità di nesting
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub iteration: usize,
    pub thought: String,
    pub tool_calls: Vec<String>,
    pub observation: String,
}

/// Sistema di gestione sub-agenti.
pub struct AgentManager {
    llm: Arc<LlmRouter>,
    mcp: Option<Arc<McpClient>>,
    /// Task attivi e completati
    tasks: Arc<Mutex<Vec<AgentTask>>>,
}

impl AgentManager {
    pub fn new(llm: Arc<LlmRouter>, mcp: Option<Arc<McpClient>>) -> Self {
        Self {
            llm,
            mcp,
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Spawna un sub-agente con un obiettivo specifico.
    /// Ritorna immediatamente con l'ID del task.
    pub async fn spawn_agent(&self, objective: &str, depth: usize) -> String {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

        if depth >= MAX_DEPTH {
            warn!(id = %id, depth = depth, "Profondità massima raggiunta");
            let mut tasks = self.tasks.lock().await;
            tasks.push(AgentTask {
                id: id.clone(),
                objective: objective.to_string(),
                status: AgentStatus::Failed,
                result: Some("Profondità massima di nesting raggiunta".to_string()),
                actions: vec![],
                depth,
            });
            return id;
        }

        let task = AgentTask {
            id: id.clone(),
            objective: objective.to_string(),
            status: AgentStatus::Running,
            result: None,
            actions: vec![],
            depth,
        };

        {
            let mut tasks = self.tasks.lock().await;
            tasks.push(task);
        }

        // Spawna il task in background
        let llm = self.llm.clone();
        let mcp = self.mcp.clone();
        let tasks = self.tasks.clone();
        let task_id = id.clone();
        let obj_owned = objective.to_string();
        let obj_log = obj_owned.clone();

        let rt_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let result = rt_handle.block_on(run_agent_loop(llm, mcp, obj_owned, depth));

            rt_handle.block_on(async {
                let mut tasks = tasks.lock().await;
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    match result {
                        Ok((text, actions)) => {
                            task.status = AgentStatus::Completed;
                            task.result = Some(text);
                            task.actions = actions;
                        }
                        Err(e) => {
                            task.status = AgentStatus::Failed;
                            task.result = Some(format!("Errore: {}", e));
                        }
                    }
                }
            });
        });

        info!(id = %id, objective = %obj_log, "Sub-agente spawnato");
        id
    }

    /// Spawna N agenti in parallelo e attende tutti i risultati.
    pub async fn spawn_parallel(&self, objectives: &[String], depth: usize) -> Vec<(String, String)> {
        let mut ids = Vec::new();
        for obj in objectives {
            let id = self.spawn_agent(obj, depth).await;
            ids.push(id);
        }

        // Attendi che tutti completino
        self.wait_all(&ids).await
    }

    /// Attende che i task specificati completino. Timeout 5 minuti.
    pub async fn wait_all(&self, ids: &[String]) -> Vec<(String, String)> {
        let timeout = tokio::time::Instant::now() + std::time::Duration::from_secs(300);

        loop {
            if tokio::time::Instant::now() > timeout {
                break;
            }

            let tasks = self.tasks.lock().await;
            let all_done = ids.iter().all(|id| {
                tasks.iter().any(|t| t.id == *id && t.status != AgentStatus::Running)
            });

            if all_done {
                return ids.iter().map(|id| {
                    let task = tasks.iter().find(|t| t.id == *id);
                    let result = task.map(|t| t.result.clone().unwrap_or_default()).unwrap_or_default();
                    (id.clone(), result)
                }).collect();
            }

            drop(tasks);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // Timeout — restituisci quello che c'è
        let tasks = self.tasks.lock().await;
        ids.iter().map(|id| {
            let task = tasks.iter().find(|t| t.id == *id);
            let result = task.map(|t| {
                if t.status == AgentStatus::Running {
                    "⏱ Timeout — task ancora in esecuzione".to_string()
                } else {
                    t.result.clone().unwrap_or_default()
                }
            }).unwrap_or_default();
            (id.clone(), result)
        }).collect()
    }

    /// Restituisce lo stato di tutti i task.
    pub async fn list_tasks(&self) -> Vec<AgentTask> {
        self.tasks.lock().await.clone()
    }

    /// Restituisce il risultato di un task specifico.
    pub async fn get_task(&self, id: &str) -> Option<AgentTask> {
        self.tasks.lock().await.iter().find(|t| t.id == id).cloned()
    }
}

/// Loop principale del sub-agente: pensa → agisce → osserva → ripensa.
/// Simile al ReAct pattern.
async fn run_agent_loop(
    llm: Arc<LlmRouter>,
    mcp: Option<Arc<McpClient>>,
    objective: String,
    depth: usize,
) -> Result<(String, Vec<AgentAction>), String> {
    let mut actions = Vec::new();
    let mut context = String::new();

    let system_prompt = format!(
        r#"Sei un sub-agente di AgentOS. Il tuo unico obiettivo è completare il task assegnato.

OBIETTIVO: {}

Lavora passo dopo passo. Ad ogni iterazione:
1. Pensa a cosa serve fare
2. Usa i tool necessari
3. Osserva il risultato
4. Decidi se hai finito o servono altri passi

Quando hai completato l'obiettivo, rispondi con:
{{"explanation": "DONE: <riassunto del risultato>", "tool_calls": []}}

Se hai bisogno di usare tool, rispondi con il formato JSON standard.

{}

REGOLE:
1. Rispondi SOLO con JSON valido.
2. Quando hai finito, metti "DONE:" all'inizio dell'explanation.
3. Sii conciso e diretto.
4. Non fare più di {} iterazioni."#,
        objective, TOOLS_DESCRIPTION, MAX_ITERATIONS
    );

    for iteration in 0..MAX_ITERATIONS {
        debug!(iteration = iteration, objective = %objective, "Sub-agente iterazione");

        // Costruisci i messaggi
        let mut messages = vec![
            ChatMessage { role: "system".to_string(), content: system_prompt.clone() },
        ];

        // Aggiungi il contesto delle iterazioni precedenti
        if !context.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!("Contesto delle azioni precedenti:\n{}\n\nContinua il lavoro. Se hai finito, rispondi con DONE.", context),
            });
        } else {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!("Inizia a lavorare sull'obiettivo: {}", objective),
            });
        }

        // Chiama LLM
        let response = llm.chat(&messages, 0.3).await
            .map_err(|e| format!("Errore LLM: {}", e))?;

        // Parsa la risposta
        let trimmed = response.content.trim();
        let (explanation, tool_calls) = parse_agent_response(trimmed);

        // Controlla se ha finito
        if explanation.starts_with("DONE:") || explanation.contains("DONE:") {
            let result = explanation.replace("DONE:", "").trim().to_string();
            actions.push(AgentAction {
                iteration,
                thought: result.clone(),
                tool_calls: vec!["(completato)".into()],
                observation: String::new(),
            });
            info!(iterations = iteration + 1, "Sub-agente completato");
            return Ok((result, actions));
        }

        // Esegui i tool
        let mut observations = Vec::new();
        let mut tool_names = Vec::new();

        for tc in &tool_calls {
            let result = tools::execute_tool_with_mcp(tc, mcp.as_ref()).await;
            tool_names.push(format!("{}({})", tc.tool, tc.args));
            observations.push(format!("[{}] {}", tc.tool,
                if result.success { &result.output } else { &result.output }
            ));
        }

        // Se non ci sono tool calls, il risultato è l'explanation
        if tool_calls.is_empty() && !explanation.is_empty() {
            actions.push(AgentAction {
                iteration,
                thought: explanation.clone(),
                tool_calls: vec![],
                observation: String::new(),
            });
            // Se non usa tool e non dice DONE, considera completato
            return Ok((explanation, actions));
        }

        let observation = observations.join("\n");
        actions.push(AgentAction {
            iteration,
            thought: explanation.clone(),
            tool_calls: tool_names,
            observation: observation.clone(),
        });

        // Aggiorna il contesto
        context.push_str(&format!("\nIterazione {}:\n  Pensiero: {}\n  Risultato: {}\n",
            iteration + 1, explanation, observation));
    }

    Err("Raggiunto il numero massimo di iterazioni".into())
}

/// Parsa la risposta dell'agente (JSON o testo libero).
fn parse_agent_response(text: &str) -> (String, Vec<ToolCall>) {
    // Prova JSON diretto
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let explanation = v.get("explanation").and_then(|e| e.as_str()).unwrap_or("").to_string();
        let tool_calls: Vec<ToolCall> = v.get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok())
            .unwrap_or_default();
        return (explanation, tool_calls);
    }

    // Prova a estrarre JSON da markdown
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) {
                let explanation = v.get("explanation").and_then(|e| e.as_str()).unwrap_or("").to_string();
                let tool_calls: Vec<ToolCall> = v.get("tool_calls")
                    .and_then(|tc| serde_json::from_value(tc.clone()).ok())
                    .unwrap_or_default();
                return (explanation, tool_calls);
            }
        }
    }

    // Fallback: testo libero senza tool
    (text.to_string(), vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_agent_response_json() {
        let (exp, calls) = parse_agent_response(r#"{"explanation":"fatto","tool_calls":[]}"#);
        assert_eq!(exp, "fatto");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_agent_response_with_tools() {
        let (exp, calls) = parse_agent_response(r#"{"explanation":"cerco","tool_calls":[{"tool":"run_command","args":{"command":"ls"}}]}"#);
        assert_eq!(exp, "cerco");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "run_command");
    }

    #[test]
    fn test_parse_agent_response_done() {
        let (exp, _) = parse_agent_response(r#"{"explanation":"DONE: tutto completato","tool_calls":[]}"#);
        assert!(exp.starts_with("DONE:"));
    }

    #[test]
    fn test_parse_agent_response_freetext() {
        let (exp, calls) = parse_agent_response("Ecco il risultato");
        assert_eq!(exp, "Ecco il risultato");
        assert!(calls.is_empty());
    }
}
