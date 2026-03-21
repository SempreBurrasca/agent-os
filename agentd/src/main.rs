//! agentd — il daemon principale di AgentOS.
//!
//! Ascolta su Unix socket per messaggi IPC da agent-shell,
//! interpreta le richieste, le pianifica, le valuta con il Guardian,
//! e le esegue in modo sicuro.

mod audit;
mod connectors;
mod context;
mod crypto;
mod executor;
mod filewatcher;
mod guardian;
mod health;
mod input;
mod intent;
mod knowledge;
mod llm;
mod mcp;
mod memory;
mod oauth;
mod planner;
mod sync;
mod tools;
mod toolforge;
mod agents;
mod voice;

use std::sync::Arc;
use std::collections::HashMap;
use anyhow::Result;
use agentos_common::config::AgentOsConfig;
use agentos_common::ipc::{AgentToShell, JsonRpcResponse};
use agentos_common::types::RiskZone;
use tracing::{info, warn, error, debug};

/// Azione in attesa di conferma dall'utente (zona gialla).
/// Contiene tutto il necessario per eseguire l'azione dopo l'approvazione.
struct PendingAction {
    /// Descrizione leggibile per l'utente
    description: String,
    /// Tool calls da eseguire se approvata
    tool_calls: Vec<tools::ToolCall>,
    /// Spiegazione dell'agente
    explanation: String,
    /// Input originale dell'utente
    original_input: String,
    /// Zona di rischio
    zone: RiskZone,
}

/// Percorso del socket IPC — /tmp per macOS, /run per Linux
#[cfg(target_os = "macos")]
const SOCKET_PATH: &str = "/tmp/agentd.sock";
#[cfg(not(target_os = "macos"))]
const SOCKET_PATH: &str = "/run/agentd.sock";

/// Percorso del file di configurazione
const CONFIG_PATH: &str = "/etc/agentos/config.yaml";

/// Directory dati dell'agente
#[cfg(target_os = "macos")]
const DATA_DIR: &str = "/tmp/agentd-data";
#[cfg(not(target_os = "macos"))]
const DATA_DIR: &str = "/var/lib/agentd";

#[tokio::main]
async fn main() -> Result<()> {
    // Inizializza il logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("agentd=debug".parse().unwrap())
        )
        .init();

    info!("AgentOS daemon avviato");

    // Carica configurazione
    let config = load_config()?;
    info!(
        default_backend = %config.llm.default_backend,
        language = %config.behavior.language,
        "Configurazione caricata"
    );

    // Inizializza i componenti
    let guardian = guardian::Guardian::new(&config.security);
    let executor = executor::Executor::new(&config.security, &config.sandbox);
    let llm_router = llm::LlmRouter::new(&config);
    let intent_engine = intent::IntentEngine::new();

    // Inizializza il knowledge graph (Feature: Auto-RAG)
    let mut knowledge_graph = knowledge::KnowledgeGraph::load();
    info!(
        entities = knowledge_graph.entity_count(),
        relations = knowledge_graph.relation_count(),
        "Knowledge graph caricato"
    );

    // Inizializza il sistema di sub-agenti (Feature: spawn_agent)
    let llm_arc = std::sync::Arc::new(llm::LlmRouter::new(&config));
    let planner = planner::Planner::new(config.behavior.max_plan_steps);

    // Inizializza memoria e contesto
    let data_dir = std::path::Path::new(DATA_DIR);
    std::fs::create_dir_all(data_dir)?;

    let memory_db_path = data_dir.join("memory.db");
    let audit_db_path = data_dir.join("audit.db");

    let memory = memory::MemoryManager::new(
        memory_db_path.to_str().unwrap()
    )?;
    let mut context = context::ContextManager::new(
        data_dir.to_str().unwrap()
    )?;
    let audit = audit::AuditTrail::new(
        audit_db_path.to_str().unwrap()
    )?;

    // Imposta permessi sicuri (600) sui file database
    // TODO: Quando si integra SQLCipher, aggiungere anche la cifratura trasparente
    crypto::ensure_secure_permissions(memory_db_path.to_str().unwrap());
    crypto::ensure_secure_permissions(audit_db_path.to_str().unwrap());

    // Verifica backend LLM disponibili
    let backends = llm_router.check_backends().await;
    for (name, available) in &backends {
        if *available {
            info!(backend = %name, "Backend LLM disponibile");
        } else {
            warn!(backend = %name, "Backend LLM non disponibile");
        }
    }

    // Inizializza il monitor di salute del sistema (Feature 3)
    let mut health_monitor = health::HealthMonitor::new(&config.ollama.url);

    // Inizializza il client MCP — connessione ai server di tool esterni
    let mcp_client = Arc::new(mcp::McpClient::new(&config.mcp).await);
    if mcp_client.tool_count() > 0 {
        info!(
            tools = mcp_client.tool_count(),
            servers = ?mcp_client.server_names(),
            "Tool MCP disponibili"
        );
    } else if !config.mcp.servers.is_empty() {
        warn!("Nessun tool MCP caricato (verifica la connettività ai server)");
    }

    // Inizializza il sistema di sub-agenti paralleli
    tools::init_agent_manager(llm_arc, Some(mcp_client.clone())).await;
    info!("Sistema sub-agenti inizializzato");

    // === File Watcher: monitora ~/Documents, ~/Desktop, ~/Downloads ===
    let mut file_event_rx = filewatcher::start(None)
        .map_err(|e| {
            warn!(error = %e, "Impossibile avviare il file watcher — continuazione senza monitoraggio file");
            e
        })
        .ok();
    if file_event_rx.is_some() {
        info!("File watcher avviato");
    }

    // === Indicizzazione iniziale (se il knowledge graph è vuoto) ===
    // Avvia la scansione in background e ricevi i risultati attraverso un canale
    let mut initial_index_rx: Option<tokio::sync::mpsc::Receiver<(String, String)>> = None;
    if knowledge_graph.entity_count() == 0 {
        info!("Knowledge graph vuoto — avvio indicizzazione iniziale in background");
        let (index_tx, index_rx) = tokio::sync::mpsc::channel::<(String, String)>(256);
        initial_index_rx = Some(index_rx);
        // Avvia l'indicizzazione in background (non blocca lo startup)
        tokio::spawn(async move {
            let count = filewatcher::initial_index(None, 3, index_tx).await;
            info!(file_processati = count, "Indicizzazione iniziale: {} file processati", count);
        });
    }

    // === Sync Manager: sincronizzazione periodica email e calendario ===
    let mut sync_manager = sync::SyncManager::new();
    if sync::SyncManager::has_email_connector() {
        info!("Connettore email rilevato — sincronizzazione periodica attiva");
    }

    // Timer per sincronizzazione email/calendario ogni 5 minuti
    let mut sync_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    sync_interval.tick().await; // consuma il tick immediato

    // Notifiche in attesa da prepend alla prossima risposta (Feature 3)
    let mut pending_notifications: Vec<String> = Vec::new();

    // Azioni in attesa di conferma utente — mappa action_id → PendingAction (Feature 3 yellow zone)
    let mut pending_actions: HashMap<String, PendingAction> = HashMap::new();

    // Genera il riepilogo della sessione precedente (Feature 2)
    if let Some(summary) = context.generate_session_summary() {
        info!("Riepilogo sessione precedente:\n{}", summary);
    }

    // Avvia l'input handler (socket IPC)
    let input_handler = input::InputHandler::new(SOCKET_PATH);
    let mut message_rx = input_handler.start().await?;

    info!("In attesa di messaggi su {}", SOCKET_PATH);

    // Gestione segnali per shutdown graceful
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate()
    )?;

    // Timer per autosave ogni 5 minuti (Feature 2)
    let mut autosave_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    autosave_interval.tick().await; // consuma il tick immediato

    // Timer per health check ogni 60 secondi (Feature 3)
    let mut health_interval = tokio::time::interval(std::time::Duration::from_secs(60));
    health_interval.tick().await; // consuma il tick immediato

    // === Loop principale ===
    loop {
        tokio::select! {
            // Messaggio ricevuto dal socket IPC
            Some(incoming) = message_rx.recv() => {
                let response = process_message(
                    incoming.message,
                    &guardian,
                    &executor,
                    &llm_router,
                    &intent_engine,
                    &planner,
                    &memory,
                    &mut context,
                    &audit,
                    &config,
                    &mut health_monitor,
                    &mut pending_notifications,
                    &mcp_client,
                    &mut pending_actions,
                    &mut knowledge_graph,
                ).await;

                // Invia la risposta al client
                let rpc_response = match response {
                    Ok(agent_msg) => {
                        let result = serde_json::to_value(&agent_msg).unwrap_or_default();
                        JsonRpcResponse::success(result, incoming.request_id)
                    }
                    Err(e) => {
                        JsonRpcResponse::error(
                            agentos_common::ipc::INTERNAL_ERROR,
                            &e.to_string(),
                            incoming.request_id,
                        )
                    }
                };

                if let Err(e) = incoming.reply_tx.send(rpc_response).await {
                    warn!(error = %e, "Errore invio risposta al client");
                }
            }

            // Autosave ogni 5 minuti (Feature 2)
            _ = autosave_interval.tick() => {
                debug!("Autosave contesto e knowledge graph");
                if let Err(e) = context.save_context() {
                    warn!(error = %e, "Errore autosave contesto");
                }
                knowledge_graph.save();
            }

            // Health check ogni 60 secondi (Feature 3)
            _ = health_interval.tick() => {
                let (_health, changes) = health_monitor.check().await;
                for change in &changes {
                    // Tenta auto-correzione (self-healing)
                    if let Some(fix_msg) = health_monitor.attempt_fix(change).await {
                        info!(fix = %fix_msg, "Self-healing eseguito");
                        pending_notifications.push(fix_msg);
                    }
                    // Genera notifica per l'utente
                    let (title, body, _urgency) = change.to_notification();
                    pending_notifications.push(format!("[{}] {}", title, body));
                }
            }

            // File watcher: file modificato → alimenta il knowledge graph
            Some((path, content)) = async {
                match file_event_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<(String, String)>>().await,
                }
            } => {
                let entities_before = knowledge_graph.entity_count();
                knowledge_graph.add_document(&path, &content);
                let entities_after = knowledge_graph.entity_count();
                let new_entities = entities_after.saturating_sub(entities_before);
                info!(
                    path = %path,
                    entita_estratte = new_entities,
                    "File modificato: {} — {} entità estratte", path, new_entities
                );
            }

            // Indicizzazione iniziale: ricevi file indicizzati in background
            Some((path, content)) = async {
                match initial_index_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<(String, String)>>().await,
                }
            } => {
                knowledge_graph.add_document(&path, &content);
                debug!(path = %path, "Indicizzazione iniziale: file processato");
            }

            // Sincronizzazione periodica email e calendario (ogni 5 minuti)
            _ = sync_interval.tick() => {
                debug!("Avvio sincronizzazione periodica email/calendario");

                // Sincronizza email
                let email_count = sync_manager.sync_emails(&mut knowledge_graph).await;
                if email_count > 0 {
                    info!(nuove_email = email_count, "Sincronizzazione: {} nuove email indicizzate", email_count);
                }

                // Sincronizza calendario
                let cal_count = sync_manager.sync_calendar(&mut knowledge_graph).await;
                if cal_count > 0 {
                    info!(eventi = cal_count, "Sincronizzazione: {} eventi calendario indicizzati", cal_count);
                }
            }

            // Segnale SIGTERM — shutdown graceful
            _ = sigterm.recv() => {
                info!("SIGTERM ricevuto — salvataggio contesto, knowledge graph e shutdown");
                context.save_context()?;
                knowledge_graph.save();
                break;
            }
        }
    }

    info!("AgentOS daemon terminato");
    Ok(())
}

/// Carica la configurazione dal file o usa i valori di default per sviluppo.
fn load_config() -> Result<AgentOsConfig> {
    // Prova il percorso di sistema
    if let Ok(config) = AgentOsConfig::from_file_with_env(CONFIG_PATH) {
        return Ok(config);
    }

    // Prova il percorso locale (sviluppo)
    if let Ok(config) = AgentOsConfig::from_file_with_env("config.yaml") {
        return Ok(config);
    }

    // Prova nella directory del progetto
    if let Ok(config) = AgentOsConfig::from_file_with_env("../config.yaml") {
        return Ok(config);
    }

    anyhow::bail!("Configurazione non trovata. Cercato in: {}, ./config.yaml, ../config.yaml", CONFIG_PATH)
}

/// Processa un singolo messaggio dalla shell.
async fn process_message(
    message: agentos_common::ipc::ShellToAgent,
    guardian: &guardian::Guardian,
    executor: &executor::Executor,
    llm_router: &llm::LlmRouter,
    intent_engine: &intent::IntentEngine,
    planner: &planner::Planner,
    memory: &memory::MemoryManager,
    context: &mut context::ContextManager,
    audit: &audit::AuditTrail,
    config: &AgentOsConfig,
    health_monitor: &mut health::HealthMonitor,
    pending_notifications: &mut Vec<String>,
    mcp_client: &Arc<mcp::McpClient>,
    pending_actions: &mut HashMap<String, PendingAction>,
    knowledge_graph: &mut knowledge::KnowledgeGraph,
) -> Result<AgentToShell> {
    use agentos_common::ipc::ShellToAgent;

    match message {
        ShellToAgent::UserInput { text } => {
            process_user_input(
                &text, guardian, executor, llm_router,
                intent_engine, planner, memory, context, audit, config,
                pending_notifications, mcp_client, pending_actions,
                knowledge_graph,
            ).await
        }

        ShellToAgent::UserConfirm { action_id, approved } => {
            info!(action_id = %action_id, approved = approved, "Conferma utente ricevuta");

            // Recupera l'azione in attesa dalla mappa
            let pending = pending_actions.remove(&action_id);
            match pending {
                Some(action) => {
                    if !approved {
                        // Utente ha rifiutato l'azione
                        audit.log_action(
                            &action.description,
                            action.zone,
                            false,
                            -1,
                            "Azione rifiutata dall'utente",
                        )?;
                        return Ok(AgentToShell::Response {
                            text: format!("Azione annullata: {}", action.description),
                            commands: None,
                            zone: Some(action.zone),
                        });
                    }

                    // Utente ha approvato — esegui le tool calls
                    info!(action_id = %action_id, "Esecuzione azione approvata");
                    let mut all_outputs = Vec::new();
                    let mut all_commands = Vec::new();

                    for tool_call in &action.tool_calls {
                        let cmd_desc = format!("{}({})", tool_call.tool, tool_call.args);
                        all_commands.push(cmd_desc.clone());

                        let result = tools::execute_tool_with_mcp(tool_call, Some(mcp_client)).await;
                        audit.log_action(
                            &format!("{}:{}", tool_call.tool, tool_call.args),
                            action.zone,
                            true,
                            if result.success { 0 } else { 1 },
                            &action.explanation,
                        )?;

                        let output_text = result.output.clone();

                        // Salva nella memoria
                        memory.add_interaction(
                            &action.original_input,
                            &output_text,
                            &[tool_call.tool.clone()],
                            result.success,
                        )?;

                        context.add_command(&cmd_desc);

                        if !output_text.is_empty() {
                            all_outputs.push(output_text);
                        }
                    }

                    let response_text = if all_outputs.is_empty() {
                        action.explanation.clone()
                    } else {
                        format!("{}\n\n{}", action.explanation, all_outputs.join("\n"))
                    };

                    context.add_conversation_entry("assistant", &response_text);

                    Ok(AgentToShell::Response {
                        text: response_text,
                        commands: Some(all_commands),
                        zone: Some(action.zone),
                    })
                }
                None => {
                    // Azione non trovata (scaduta o ID errato)
                    warn!(action_id = %action_id, "Azione in attesa non trovata");
                    Ok(AgentToShell::Response {
                        text: format!("Azione con ID {} non trovata (potrebbe essere scaduta).", action_id),
                        commands: None,
                        zone: None,
                    })
                }
            }
        }

        ShellToAgent::WindowFocus { window_id, app_name, title } => {
            context.update_windows(vec![
                context::WindowInfo { window_id, app_name, title }
            ]);
            Ok(AgentToShell::Response {
                text: String::new(),
                commands: None,
                zone: None,
            })
        }

        ShellToAgent::BriefingRequest => {
            // Feature 4: briefing completo con salute, file recenti, sessione, pattern
            let mut briefing_parts: Vec<String> = Vec::new();

            // Stato salute sistema
            let (health_status, _changes) = health_monitor.check().await;
            let net_status = match health_status.network {
                health::NetworkState::Connected => "connessa",
                health::NetworkState::Disconnected => "disconnessa",
                health::NetworkState::Unknown => "sconosciuta",
            };
            let mut health_line = format!("Rete: {}", net_status);
            if let Some(disk) = health_status.disk_usage_pct {
                health_line.push_str(&format!(" | Disco: {}%", disk));
            }
            if let Some(mem) = health_status.mem_available_mb {
                health_line.push_str(&format!(" | RAM libera: {} MB", mem));
            }
            if health_status.ollama_available {
                health_line.push_str(" | Ollama: attivo");
            }
            briefing_parts.push(format!("Sistema: {}", health_line));

            // Email — se il connettore è configurato, mostra conteggio non lette e i primi 3 oggetti
            let email_summary = get_email_briefing(config).await;
            if !email_summary.is_empty() {
                briefing_parts.push(format!("Email:\n{}", email_summary));
            }

            // Calendario — se il connettore è configurato, mostra i prossimi 3 eventi
            let calendar_summary = get_calendar_briefing(config).await;
            if !calendar_summary.is_empty() {
                briefing_parts.push(format!("Calendario:\n{}", calendar_summary));
            }

            // Riepilogo sessione precedente
            if let Some(summary) = context.generate_session_summary() {
                briefing_parts.push(format!("Sessione precedente:\n{}", summary));
            }

            // File recenti dalla home (ultimi 5 modificati)
            let recent_files = get_recent_files().await;
            if !recent_files.is_empty() {
                briefing_parts.push(format!("File recenti:\n{}", recent_files.join("\n")));
            }

            // Pattern appresi
            if let Ok(patterns) = memory.get_relevant_patterns() {
                if !patterns.is_empty() {
                    let pattern_lines: Vec<String> = patterns.iter()
                        .take(3)
                        .map(|p| format!("  - [{}] {}", p.pattern_type, p.pattern_data))
                        .collect();
                    briefing_parts.push(format!("Pattern appresi:\n{}", pattern_lines.join("\n")));
                }
            }

            let briefing_text = if briefing_parts.is_empty() {
                "Nessuna informazione disponibile per il briefing.".to_string()
            } else {
                briefing_parts.join("\n\n")
            };

            // Restituisci come Response con il briefing completo
            // (BriefingUpdate richiede dati strutturati che non abbiamo ancora)
            Ok(AgentToShell::Response {
                text: briefing_text,
                commands: None,
                zone: None,
            })
        }

        ShellToAgent::SearchRequest { query } => {
            // Deleghiamo al tool semantic_search
            let call = tools::ToolCall {
                tool: "semantic_search".to_string(),
                args: serde_json::json!({"query": query}),
            };
            let result = tools::execute_tool(&call).await;
            Ok(AgentToShell::Response {
                text: result.output,
                commands: None,
                zone: None,
            })
        }

        ShellToAgent::WorkspaceModeChange { mode } => {
            info!(mode = ?mode, "Cambio modalità workspace");
            Ok(AgentToShell::Response {
                text: String::new(),
                commands: None,
                zone: None,
            })
        }
    }
}

/// Recupera i file modificati di recente nella home (per il briefing).
async fn get_recent_files() -> Vec<String> {
    // Su macOS usiamo find con -mmin, su Linux uguale
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
    let cmd = format!(
        "find {} -maxdepth 3 -type f -mmin -1440 -not -path '*/.*' 2>/dev/null | head -5",
        home
    );

    match tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await {
        Ok(output) => {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|l| format!("  - {}", l))
                .collect()
        }
        Err(_) => vec![],
    }
}

/// Recupera un riepilogo delle email non lette (se il connettore è configurato).
/// TODO: Collegare al connettore email reale (Google/Outlook) quando i token sono disponibili.
async fn get_email_briefing(_config: &AgentOsConfig) -> String {
    // Per ora restituisce stringa vuota — il connettore email richiede token OAuth
    // che non sono ancora integrati nel flusso principale.
    String::new()
}

/// Recupera un riepilogo degli eventi calendario (se il connettore è configurato).
/// TODO: Collegare al connettore calendario reale (Google/Outlook) quando i token sono disponibili.
async fn get_calendar_briefing(_config: &AgentOsConfig) -> String {
    // Per ora restituisce stringa vuota — il connettore calendario richiede token OAuth.
    String::new()
}

/// Processa l'input testuale dell'utente: intent → plan → guardian → execute.
async fn process_user_input(
    text: &str,
    guardian: &guardian::Guardian,
    _executor: &executor::Executor,
    llm_router: &llm::LlmRouter,
    intent_engine: &intent::IntentEngine,
    _planner: &planner::Planner,
    memory: &memory::MemoryManager,
    context: &mut context::ContextManager,
    audit: &audit::AuditTrail,
    _config: &AgentOsConfig,
    pending_notifications: &mut Vec<String>,
    mcp_client: &Arc<mcp::McpClient>,
    pending_actions: &mut HashMap<String, PendingAction>,
    knowledge_graph: &mut knowledge::KnowledgeGraph,
) -> Result<AgentToShell> {
    info!(input = text, "Processamento input utente");

    // Salva nella conversazione
    context.add_conversation_entry("user", text);

    // Feature 1: recupera interazioni recenti e pattern per il contesto multi-turno
    let recent_interactions: Vec<(String, String)> = memory.get_recent(5)
        .unwrap_or_default()
        .iter()
        .rev() // ordine cronologico (get_recent restituisce DESC)
        .map(|i| (i.user_input.clone(), i.agent_response.clone()))
        .collect();

    let patterns: Vec<String> = memory.get_relevant_patterns()
        .unwrap_or_default()
        .iter()
        .map(|p| format!("[{}] {}", p.pattern_type, p.pattern_data))
        .collect();

    // Auto-RAG: arricchisci l'input con contesto dal knowledge graph
    let kg_context = knowledge_graph.format_context(text);
    let enriched_input = if kg_context.is_empty() {
        text.to_string()
    } else {
        debug!(entities = knowledge_graph.entity_count(), "Auto-RAG: contesto trovato nel knowledge graph");
        format!("{}\n\n{}", text, kg_context)
    };

    // Alimenta il knowledge graph con l'input dell'utente
    knowledge_graph.add_document("chat", text);

    // 1. Interpreta con il tool system (chiamata singola — rimosso duplicato)
    // Includi anche i tool MCP nella descrizione per l'LLM
    let mcp_desc = mcp_client.tools_description();
    let agent_resp = match intent_engine.interpret_with_mcp(&enriched_input, llm_router, &recent_interactions, &patterns, &mcp_desc).await {
        Ok(resp) => resp,
        Err(e) => {
            warn!(error = %e, "Errore interpretazione");
            return Ok(AgentToShell::Response {
                text: "Mi dispiace, non sono riuscito a elaborare la richiesta. Puoi riformulare?".to_string(),
                commands: None,
                zone: None,
            });
        }
    };

    // Se non ci sono tool call, rispondi direttamente
    if agent_resp.tool_calls.is_empty() {
        // Salva interazione anche per risposte senza tool
        memory.add_interaction(text, &agent_resp.explanation, &[], true)?;
        context.add_conversation_entry("assistant", &agent_resp.explanation);

        // Prepend notifiche in sospeso (Feature 3)
        let response_text = prepend_notifications(&agent_resp.explanation, pending_notifications);
        return Ok(AgentToShell::Response {
            text: response_text,
            commands: None,
            zone: None,
        });
    }

    // 2. Valuta i tool call con il Guardian — prima passata per determinare la zona
    let mut overall_zone = RiskZone::Green;
    let mut yellow_descriptions = Vec::new();

    for tool_call in &agent_resp.tool_calls {
        // Estrai il comando per il Guardian (se è run_command o install_package)
        let cmd_to_check = match tool_call.tool.as_str() {
            "run_command" => tool_call.args.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "install_package" => format!("apt install {}", tool_call.args.get("package").and_then(|v| v.as_str()).unwrap_or("")),
            _ => String::new(),
        };

        // Verifica col Guardian
        if !cmd_to_check.is_empty() {
            let verdict = guardian.evaluate(&cmd_to_check);
            if verdict.blocked {
                audit.log_action(&cmd_to_check, RiskZone::Red, false, -1, &verdict.reason)?;
                return Ok(AgentToShell::Response {
                    text: format!("{}\n\n⛔ Bloccato dal Guardian: {}", agent_resp.explanation, verdict.reason),
                    commands: Some(vec![cmd_to_check]),
                    zone: Some(RiskZone::Red),
                });
            }
            if verdict.zone == RiskZone::Yellow {
                overall_zone = RiskZone::Yellow;
                yellow_descriptions.push(format!("{} ({})", cmd_to_check, verdict.reason));
            }
        }
    }

    // Feature 3: se la zona è gialla, salva le azioni e chiedi conferma all'utente
    if overall_zone == RiskZone::Yellow {
        let action_id = uuid::Uuid::new_v4().to_string();
        let description = format!(
            "{}\nComandi: {}",
            agent_resp.explanation,
            yellow_descriptions.join(", ")
        );

        info!(action_id = %action_id, "Azione in zona gialla — richiesta conferma utente");

        // Salva l'azione in attesa nella mappa
        pending_actions.insert(action_id.clone(), PendingAction {
            description: description.clone(),
            tool_calls: agent_resp.tool_calls.clone(),
            explanation: agent_resp.explanation.clone(),
            original_input: text.to_string(),
            zone: RiskZone::Yellow,
        });

        // Restituisci una richiesta di conferma al client
        return Ok(AgentToShell::ConfirmRequest {
            action_id,
            description,
            zone: RiskZone::Yellow,
        });
    }

    // 3. Zona verde — esegui direttamente le tool calls
    let mut all_outputs = Vec::new();
    let mut all_commands = Vec::new();

    for tool_call in &agent_resp.tool_calls {
        let cmd_desc = format!("{}({})", tool_call.tool, tool_call.args);
        all_commands.push(cmd_desc.clone());

        // Esegui il tool (con supporto MCP)
        let result = tools::execute_tool_with_mcp(tool_call, Some(mcp_client)).await;
        audit.log_action(
            &format!("{}:{}", tool_call.tool, tool_call.args),
            overall_zone,
            true,
            if result.success { 0 } else { 1 },
            &agent_resp.explanation,
        )?;

        let output_text = result.output.clone();

        // Salva nella memoria
        memory.add_interaction(
            text,
            &output_text,
            &[tool_call.tool.clone()],
            result.success,
        )?;

        // Salva comandi eseguiti nel contesto
        context.add_command(&cmd_desc);

        // Alimenta il knowledge graph con l'output dei tool che leggono/creano contenuti
        if result.success && !output_text.is_empty() {
            let source = match tool_call.tool.as_str() {
                "read_file" => tool_call.args.get("path").and_then(|v| v.as_str()).unwrap_or("file").to_string(),
                "list_emails" | "read_email" | "search_emails" => "email".to_string(),
                "list_events" => "calendario".to_string(),
                "browse_url" => tool_call.args.get("url").and_then(|v| v.as_str()).unwrap_or("web").to_string(),
                "web_search" => "web_search".to_string(),
                "semantic_search" => "search".to_string(),
                _ => String::new(),
            };
            if !source.is_empty() {
                knowledge_graph.add_document(&source, &output_text);
            }
        }

        if !output_text.is_empty() {
            all_outputs.push(output_text);
        }
    }

    // Rileva pattern dopo ogni interazione con tool
    if let Err(e) = memory.detect_patterns() {
        debug!(error = %e, "Errore rilevamento pattern");
    }

    // 4. Componi la risposta
    let response_text = if all_outputs.is_empty() {
        agent_resp.explanation.clone()
    } else {
        format!("{}\n\n{}", agent_resp.explanation, all_outputs.join("\n"))
    };

    context.add_conversation_entry("assistant", &response_text);

    // Prepend notifiche in sospeso
    let final_text = prepend_notifications(&response_text, pending_notifications);

    Ok(AgentToShell::Response {
        text: final_text,
        commands: Some(all_commands),
        zone: Some(overall_zone),
    })
}

/// Prepend le notifiche in sospeso alla risposta e svuota il buffer.
fn prepend_notifications(response: &str, notifications: &mut Vec<String>) -> String {
    if notifications.is_empty() {
        return response.to_string();
    }

    let mut text = String::new();
    text.push_str("📋 Notifiche:\n");
    for notif in notifications.iter() {
        text.push_str(&format!("  • {}\n", notif));
    }
    text.push('\n');
    text.push_str(response);

    notifications.clear();
    text
}
