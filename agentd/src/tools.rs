//! Tool system — capacità dell'agente per interagire col sistema.
//!
//! Ogni tool è un'azione atomica che l'agente può invocare.
//! Il sistema di tool viene presentato all'LLM come parte del system prompt
//! così l'agente sa cosa può fare e genera le chiamate giuste.

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{info, debug, warn};

use crate::mcp::McpClient;
use crate::voice::VoiceInput;
use crate::toolforge::{ToolForge, ToolParam};
use crate::agents::AgentManager;
use once_cell::sync::Lazy;
use tokio::sync::Mutex as TokioMutex;

/// Registro globale dei tool custom.
static TOOL_FORGE: Lazy<TokioMutex<ToolForge>> = Lazy::new(|| TokioMutex::new(ToolForge::load()));

/// Agent manager globale.
static AGENT_MANAGER: Lazy<TokioMutex<Option<Arc<AgentManager>>>> = Lazy::new(|| TokioMutex::new(None));

/// Inizializza l'agent manager con il LLM router. Chiamato da main.rs all'avvio.
pub async fn init_agent_manager(llm: Arc<crate::llm::LlmRouter>, mcp: Option<Arc<McpClient>>) {
    let mgr = AgentManager::new(llm, mcp);
    *AGENT_MANAGER.lock().await = Some(Arc::new(mgr));
}

/// Risultato dell'esecuzione di un tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: String,
    pub success: bool,
    pub output: String,
    pub data: Option<serde_json::Value>,
}

/// Chiamata a un tool dall'LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value,
}

/// Esegue un tool e restituisce il risultato.
/// Se è presente un client MCP, i tool MCP vengono gestiti tramite proxy.
pub async fn execute_tool(call: &ToolCall) -> ToolResult {
    execute_tool_with_mcp(call, None).await
}

/// Esegue un tool con supporto MCP opzionale.
/// Se `mcp_client` è Some e il tool è un tool MCP, la chiamata viene delegata al server.
pub async fn execute_tool_with_mcp(call: &ToolCall, mcp_client: Option<&Arc<McpClient>>) -> ToolResult {
    // Prima controlla se è un tool built-in
    match call.tool.as_str() {
        "run_command" => return tool_run_command(call).await,
        "launch_app" => return tool_launch_app(call).await,
        "install_package" => return tool_install_package(call).await,
        "search_files" => return tool_search_files(call).await,
        "read_file" => return tool_read_file(call).await,
        "system_info" => return tool_system_info(call).await,
        "list_files" => return tool_list_files(call).await,
        "check_installed" => return tool_check_installed(call).await,
        "semantic_search" => return tool_semantic_search(call).await,
        "browse_url" => return tool_browse_url(call).await,
        "web_search" => return tool_web_search(call).await,
        "start_agent_fs" => return tool_start_agent_fs(call).await,
        "agent_fs_status" => return tool_agent_fs_status(call).await,
        "voice_listen" => return tool_voice_listen(call).await,
        "voice_speak" => return tool_voice_speak(call).await,
        "list_emails" => return tool_list_emails(call).await,
        "read_email" => return tool_read_email(call).await,
        "search_emails" => return tool_search_emails(call).await,
        "list_events" => return tool_list_events(call).await,
        "create_event" => return tool_create_event(call).await,
        // Auto-sviluppo — l'agente crea i propri tool e app
        "create_tool" => return tool_create_tool(call).await,
        "create_app" => return tool_create_app(call).await,
        "edit_file" => return tool_edit_file(call).await,
        "list_custom_tools" => return tool_list_custom_tools(call).await,
        // Connessione OAuth zero-config
        "connect_service" => return tool_connect_service(call).await,
        // Sub-agenti paralleli
        "spawn_agent" => return tool_spawn_agent(call).await,
        "spawn_parallel" => return tool_spawn_parallel(call).await,
        "list_agents" => return tool_list_agents(call).await,
        _ => {
            // Controlla se è un tool custom (prefisso custom_)
            if call.tool.starts_with("custom_") {
                let tool_name = call.tool.strip_prefix("custom_").unwrap_or(&call.tool);
                // Copia i dati necessari dal lock, poi esegui fuori dal lock
                let tool_info = {
                    let forge = TOOL_FORGE.lock().await;
                    forge.get_tool_info(tool_name).map(|(lang, path)| (lang, path))
                };
                if let Some((language, script_path)) = tool_info {
                    let args_json = serde_json::to_string(&call.args).unwrap_or_default();
                    let interpreter = match language.as_str() { "python"|"py" => "python3", "bash"|"sh" => "bash", "node"|"javascript"|"js" => "node", _ => "sh" };
                    let output = Command::new(interpreter).arg(&script_path).env("TOOL_ARGS", &args_json).env("TOOL_NAME", tool_name).output().await;
                    // Incrementa uso
                    { let mut f = TOOL_FORGE.lock().await; f.increment_use(tool_name); }
                    return match output {
                        Ok(o) if o.status.success() => ToolResult { tool: call.tool.clone(), success: true, output: String::from_utf8_lossy(&o.stdout).to_string(), data: None },
                        Ok(o) => ToolResult { tool: call.tool.clone(), success: false, output: String::from_utf8_lossy(&o.stderr).to_string(), data: None },
                        Err(e) => ToolResult { tool: call.tool.clone(), success: false, output: format!("Errore: {}", e), data: None },
                    };
                }
            }
        }
    }

    // Se non è built-in, prova come tool MCP
    if let Some(mcp) = mcp_client {
        if mcp.is_mcp_tool(&call.tool) {
            return tool_mcp_proxy(call, mcp).await;
        }
    }

    ToolResult {
        tool: call.tool.clone(),
        success: false,
        output: format!("Tool '{}' non riconosciuto", call.tool),
        data: None,
    }
}

/// Descrizione dei tool disponibili per il system prompt LLM.
pub const TOOLS_DESCRIPTION: &str = r#"
IMPORTANTE: Rispondi SEMPRE e SOLO con un oggetto JSON valido. Mai testo libero. Mai markdown fuori dal JSON.

TOOL:
- run_command: {"command": "..."}  — esegue un comando shell
- launch_app: {"app": "..."}  — lancia app (terminale, browser, file-manager, editor)
- install_package: {"package": "..."}  — installa con apt
- search_files: {"query": "...", "path": "...", "type": "..."}  — cerca file PER NOME (find + grep)
- semantic_search: {"query": "...", "path": "...", "type": "..."}  — cerca file PER SIGNIFICATO/CONTENUTO (ricerca semantica via agent-fs)
- read_file: {"path": "..."}  — legge un file
- system_info: {"what": "all|memory|disk|network|cpu"}  — info sistema
- list_files: {"path": "...", "recursive": false}  — elenca file
- check_installed: {"program": "..."}  — verifica se installato
- browse_url: {"url": "..."}  — naviga un URL e restituisce il contenuto testuale della pagina (primi 5000 caratteri)
- web_search: {"query": "...", "max_results": 5}  — cerca sul web e restituisce i risultati (titolo, URL, snippet)
- start_agent_fs: {}  — avvia il servizio agent-fs per indicizzazione semantica dei file
- agent_fs_status: {}  — controlla lo stato del servizio agent-fs
- voice_listen: {"duration": 5}  — registra audio dal microfono per N secondi e trascrive con Whisper (default 5 sec)
- voice_speak: {"text": "..."}  — sintetizza il testo in voce con macOS `say` (TTS)
- list_emails: {"max_results": 10}  — elenca le email recenti dalla casella configurata (Google/Outlook)
- read_email: {"id": "..."}  — legge una singola email per ID (restituisce mittente, oggetto, anteprima)
- search_emails: {"query": "...", "max_results": 10}  — cerca email per parole chiave
- list_events: {"days": 7, "max_results": 10}  — elenca i prossimi eventi del calendario
- create_event: {"title": "...", "start": "2026-03-20T10:00:00Z", "end": "2026-03-20T11:00:00Z", "location": "...", "description": "..."}  — crea un evento nel calendario
- connect_service: {"provider": "gmail|outlook"}  — connetti un servizio (apre il browser per autenticazione OAuth, zero-config)

QUANDO USARE connect_service:
- Quando l'utente chiede di connettere email, Gmail, Outlook, calendario, o dice "connetti", "collega", "autentica"
- Il flusso apre il browser automaticamente, l'utente autorizza, e i token vengono salvati
- Dopo la connessione, i tool email e calendario funzioneranno automaticamente
- Provider supportati: gmail (Google), outlook (Microsoft)

QUANDO USARE I TOOL EMAIL E CALENDARIO:
- list_emails: quando l'utente chiede le email recenti, la posta in arrivo, o quante email non lette ha
- read_email: quando l'utente vuole leggere una specifica email (usa l'ID ottenuto da list_emails o search_emails)
- search_emails: quando l'utente cerca email su un argomento specifico (es. "email dal dentista", "fatture di gennaio")
- list_events: quando l'utente chiede gli appuntamenti, la sua agenda, i prossimi impegni
- create_event: quando l'utente vuole creare un appuntamento o evento nel calendario

DIFFERENZA TRA search_files E semantic_search:
- search_files: cerca per NOME file (es. "fattura*.pdf", file che si chiamano "relazione")
- semantic_search: cerca per SIGNIFICATO/CONTENUTO (es. "documenti sulla privacy", "email dal dentista")
  Se l'utente cerca qualcosa per contenuto o concetto, usa semantic_search.
  Se l'utente cerca un file specifico per nome, usa search_files.

AUTO-SVILUPPO — puoi creare nuovi tool e app:
- create_tool: {"name": "...", "description": "...", "language": "python|bash|node", "code": "...", "parameters": [{"name": "...", "description": "...", "required": true}]}
  Crea un nuovo tool come script. Lo script riceve gli argomenti nella variabile d'ambiente TOOL_ARGS (JSON).
  Il tool viene salvato e sarà disponibile nelle sessioni future come custom_<nome>.
- create_app: {"name": "...", "description": "...", "language": "...", "files": {"main.py": "codice...", "README.md": "..."}}
  Crea un'app completa con più file. Salvata in ~/.agentos/apps/<nome>/
- edit_file: {"path": "...", "content": "..."}  — sovrascrive il contenuto di un file
  Oppure: {"path": "...", "find": "testo_da_trovare", "replace": "testo_sostitutivo"} — find/replace
- list_custom_tools: {}  — elenca i tool custom creati

SUB-AGENTI PARALLELI — per task complessi, puoi delegare a sub-agenti che lavorano in parallelo:
- spawn_agent: {"objective": "..."}  — crea un sub-agente con un obiettivo specifico. Ritorna l'ID.
  Il sub-agente lavora autonomamente usando tutti i tool e restituisce il risultato.
- spawn_parallel: {"objectives": ["task1", "task2", "task3"]}  — crea N sub-agenti in parallelo e attende tutti i risultati.
  USALO quando devi fare più cose indipendenti contemporaneamente.
- list_agents: {}  — elenca i sub-agenti attivi e completati

QUANDO USARE I SUB-AGENTI:
- Task complessi con parti indipendenti (es. "crea un'app con frontend e backend" → 2 agenti paralleli)
- Ricerche multiple (es. "confronta i prezzi su 3 siti" → 3 agenti paralleli)
- Analisi parallele (es. "analizza questi 5 file" → 5 agenti paralleli)
- Quando il task richiede più di 3-4 tool calls sequenziali, considera di delegare a un sub-agente.

IMPORTANTE: Quando ti manca una capacità per completare un task, CREA UN TOOL per risolverlo.
Esempio: se l'utente chiede di convertire PDF in testo e non hai un tool, crea uno script Python che lo fa.

QUANDO USARE browse_url E web_search:
- web_search: quando l'utente chiede informazioni aggiornate, notizie, o qualcosa che richiede una ricerca sul web
- browse_url: quando l'utente fornisce un URL specifico da leggere, o dopo una web_search per approfondire un risultato
  Usa web_search per trovare le pagine, browse_url per leggerne il contenuto.

ESEMPI (segui ESATTAMENTE questo formato):

Utente: "apri il terminale"
{"explanation":"Apro il terminale.","tool_calls":[{"tool":"launch_app","args":{"app":"terminale"}}]}

Utente: "mostrami i file"
{"explanation":"Ecco i file nella directory corrente.","tool_calls":[{"tool":"run_command","args":{"command":"ls -la"}}]}

Utente: "installa gimp"
{"explanation":"Installo GIMP per te.","tool_calls":[{"tool":"install_package","args":{"package":"gimp"}}]}

Utente: "quanta RAM ho?"
{"explanation":"Controllo la memoria del sistema.","tool_calls":[{"tool":"system_info","args":{"what":"memory"}}]}

Utente: "apri il browser"
{"explanation":"Apro il browser web.","tool_calls":[{"tool":"launch_app","args":{"app":"browser"}}]}

Utente: "cerca il file fattura.pdf"
{"explanation":"Cerco il file per nome.","tool_calls":[{"tool":"search_files","args":{"query":"fattura","path":"/home","type":"pdf"}}]}

Utente: "trova documenti che parlano del progetto Alpha"
{"explanation":"Cerco nei contenuti dei file per significato.","tool_calls":[{"tool":"semantic_search","args":{"query":"progetto Alpha","path":"/home"}}]}

Utente: "crea un file con scritto ciao"
{"explanation":"Creo il file per te.","tool_calls":[{"tool":"run_command","args":{"command":"echo 'ciao' > /tmp/ciao.txt"}}]}

Utente: "scrivi i termini e condizioni per il mio sito"
{"explanation":"Creo un file con i termini e condizioni e lo apro nell'editor.","tool_calls":[{"tool":"run_command","args":{"command":"cat > /tmp/termini.txt << 'EOF'\nTermini e Condizioni\n\n1. Introduzione\nQuesti termini regolano l'uso del sito.\n\n2. Uso del servizio\nL'utente si impegna a usare il servizio in modo lecito.\n\n3. Privacy\nI dati personali sono trattati secondo la normativa GDPR.\nEOF"}},{"tool":"launch_app","args":{"app":"editor"}}]}

Utente: "che ore sono?"
{"explanation":"Sono le 14:30.","tool_calls":[{"tool":"run_command","args":{"command":"date '+%H:%M'"}}]}

Utente: "ho bisogno di un tool che converta immagini in PDF"
{"explanation":"Creo un tool Python per convertire immagini in PDF.","tool_calls":[{"tool":"create_tool","args":{"name":"img_to_pdf","description":"Converte un'immagine in PDF","language":"python","code":"import os, json, subprocess\nargs = json.loads(os.environ.get('TOOL_ARGS', '{}'))\ninput_path = args.get('input', '')\noutput_path = args.get('output', input_path.rsplit('.', 1)[0] + '.pdf')\nsubprocess.run(['convert', input_path, output_path], check=True)\nprint(f'Convertito: {output_path}')","parameters":[{"name":"input","description":"percorso immagine","required":true},{"name":"output","description":"percorso PDF output","required":false}]}}]}

Utente: "crea un sito web con homepage e pagina about, tutto in parallelo"
{"explanation":"Delego a due sub-agenti paralleli per creare le pagine.","tool_calls":[{"tool":"spawn_parallel","args":{"objectives":["Crea il file /tmp/site/index.html con una homepage moderna dark mode con hero section","Crea il file /tmp/site/about.html con una pagina about professionale dark mode"]}}]}

Utente: "analizza il codice in src/ e crea un report"
{"explanation":"Delego a un sub-agente per l'analisi approfondita.","tool_calls":[{"tool":"spawn_agent","args":{"objective":"Analizza tutti i file .rs in src/, conta le righe di codice, identifica i moduli principali, e crea un report in /tmp/report.md"}}]}

Utente: "modifica il file /tmp/test.txt e sostituisci hello con ciao"
{"explanation":"Modifico il file sostituendo hello con ciao.","tool_calls":[{"tool":"edit_file","args":{"path":"/tmp/test.txt","find":"hello","replace":"ciao"}}]}

Utente: "ciao, come stai?"
{"explanation":"Ciao! Tutto bene, sono pronto ad aiutarti. Chiedimi qualsiasi cosa: posso eseguire comandi, installare programmi, cercare file, e molto altro.","tool_calls":[]}

Utente: "quali sono le ultime notizie su Rust?"
{"explanation":"Cerco le ultime notizie su Rust sul web.","tool_calls":[{"tool":"web_search","args":{"query":"ultime notizie Rust programming language 2026"}}]}

Utente: "leggi il contenuto di https://example.com"
{"explanation":"Recupero il contenuto della pagina.","tool_calls":[{"tool":"browse_url","args":{"url":"https://example.com"}}]}

Utente: "cos'è il Model Context Protocol?"
{"explanation":"Cerco informazioni sul Model Context Protocol.","tool_calls":[{"tool":"web_search","args":{"query":"Model Context Protocol MCP cos'è"}}]}

Utente: "mostrami le email"
{"explanation":"Ecco le email recenti.","tool_calls":[{"tool":"list_emails","args":{"max_results":10}}]}

Utente: "ho email non lette?"
{"explanation":"Controllo le email non lette.","tool_calls":[{"tool":"list_emails","args":{"max_results":5}}]}

Utente: "cerca email dal dentista"
{"explanation":"Cerco email relative al dentista.","tool_calls":[{"tool":"search_emails","args":{"query":"dentista","max_results":10}}]}

Utente: "che appuntamenti ho questa settimana?"
{"explanation":"Ecco gli appuntamenti dei prossimi 7 giorni.","tool_calls":[{"tool":"list_events","args":{"days":7,"max_results":10}}]}

Utente: "crea un appuntamento domani alle 10 per riunione"
{"explanation":"Creo l'evento nel calendario.","tool_calls":[{"tool":"create_event","args":{"title":"Riunione","start":"2026-03-21T10:00:00Z","end":"2026-03-21T11:00:00Z"}}]}

Utente: "connetti la mia email gmail"
{"explanation":"Avvio la connessione a Gmail. Si aprirà il browser per l'autorizzazione.","tool_calls":[{"tool":"connect_service","args":{"provider":"gmail"}}]}

Utente: "collega outlook"
{"explanation":"Avvio la connessione a Outlook.","tool_calls":[{"tool":"connect_service","args":{"provider":"outlook"}}]}

REGOLE:
1. Rispondi SOLO con JSON. Niente testo prima o dopo il JSON.
2. Se servono azioni, metti i tool in tool_calls. Se è solo conversazione, tool_calls vuoto [].
3. Per creare contenuti (documenti, codice), scrivi il contenuto in un file con run_command e poi apri l'editor.
4. Explanation deve essere BREVE (1-2 righe). Il dettaglio va nei tool.
5. Per domande che richiedono info aggiornate dal web, usa web_search. Per leggere pagine specifiche, usa browse_url.
6. Per email e calendario, usa i tool dedicati (list_emails, search_emails, list_events, create_event). Funzionano solo se l'utente ha configurato un connettore Google o Microsoft.
"#;

// ============================================================
// Implementazioni tool
// ============================================================

async fn tool_run_command(call: &ToolCall) -> ToolResult {
    let cmd = call.args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if cmd.is_empty() {
        return ToolResult { tool: "run_command".into(), success: false, output: "Comando vuoto".into(), data: None };
    }

    debug!(command = cmd, "Esecuzione comando");
    match Command::new("sh").arg("-c").arg(cmd).output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = if stderr.is_empty() { stdout } else { format!("{}\n{}", stdout, stderr) };
            ToolResult {
                tool: "run_command".into(),
                success: output.status.success(),
                output: combined,
                data: Some(serde_json::json!({"exit_code": output.status.code()})),
            }
        }
        Err(e) => ToolResult {
            tool: "run_command".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

async fn tool_launch_app(call: &ToolCall) -> ToolResult {
    let app = call.args.get("app").and_then(|v| v.as_str()).unwrap_or("");
    if app.is_empty() {
        return ToolResult { tool: "launch_app".into(), success: false, output: "App non specificata".into(), data: None };
    }

    // Mappa alias comuni ai binari corretti per X11
    let resolved = match app {
        "terminale" | "terminal" | "term" => "foot",
        "browser" | "web" | "firefox" => "firefox",
        "file-manager" | "files" | "nautilus" | "thunar" => "thunar",
        "editor" | "text-editor" | "notepad" => "mousepad",
        "monitor" | "task-manager" | "htop" => "foot -e htop",
        other => other,
    };

    // Verifica se è installata
    let check = Command::new("which").arg(resolved).output().await;
    if check.map(|o| !o.status.success()).unwrap_or(true) {
        return ToolResult {
            tool: "launch_app".into(), success: false,
            output: format!("'{}' non è installato. Usa install_package per installarlo prima.", resolved),
            data: Some(serde_json::json!({"not_installed": true, "app": resolved})),
        };
    }

    info!(app = resolved, "Lancio applicazione");

    // Determina l'environment: Wayland (sway) o X11 fallback
    let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let use_wayland = !wayland_display.is_empty() || std::path::Path::new("/tmp/xdg-root/wayland-1").exists();

    let mut cmd = if resolved.contains(' ') {
        let mut c = Command::new("sh");
        c.arg("-c").arg(resolved);
        c
    } else {
        Command::new(resolved)
    };

    cmd.env("XDG_RUNTIME_DIR", "/tmp/xdg-root");
    cmd.env("DISPLAY", ":1");
    if use_wayland {
        let wd = if wayland_display.is_empty() { "wayland-1".to_string() } else { wayland_display };
        cmd.env("WAYLAND_DISPLAY", &wd);
    }
    // Firefox snap ha problemi con Wayland in VNC — forza X11
    if resolved.contains("firefox") {
        cmd.env("MOZ_ENABLE_WAYLAND", "0");
        cmd.env("WAYLAND_DISPLAY", "");
    }

    match cmd.spawn()
    {
        Ok(child) => {
            let pid = child.id().unwrap_or(0);
            ToolResult {
                tool: "launch_app".into(), success: true,
                output: format!("{} avviato (PID {})", resolved, pid),
                data: Some(serde_json::json!({"pid": pid, "app": resolved})),
            }
        }
        Err(e) => ToolResult {
            tool: "launch_app".into(), success: false,
            output: format!("Errore avvio {}: {}", resolved, e), data: None,
        },
    }
}

async fn tool_install_package(call: &ToolCall) -> ToolResult {
    let pkg = call.args.get("package").and_then(|v| v.as_str()).unwrap_or("");
    if pkg.is_empty() {
        return ToolResult { tool: "install_package".into(), success: false, output: "Pacchetto non specificato".into(), data: None };
    }

    info!(package = pkg, "Installazione pacchetto");
    match Command::new("apt-get")
        .args(["install", "-y", pkg])
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output().await
    {
        Ok(output) => {
            let out = String::from_utf8_lossy(&output.stdout).to_string();
            let err = String::from_utf8_lossy(&output.stderr).to_string();
            ToolResult {
                tool: "install_package".into(),
                success: output.status.success(),
                output: if output.status.success() {
                    format!("{} installato con successo", pkg)
                } else {
                    format!("Errore installazione: {}", err)
                },
                data: Some(serde_json::json!({"package": pkg})),
            }
        }
        Err(e) => ToolResult {
            tool: "install_package".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

async fn tool_search_files(call: &ToolCall) -> ToolResult {
    let query = call.args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("/home");
    let file_type = call.args.get("type").and_then(|v| v.as_str());

    let mut cmd_str = format!("find {} -maxdepth 5", path);
    if let Some(ft) = file_type {
        cmd_str.push_str(&format!(" -name '*.{}'", ft));
    }
    // Cerca per nome che contiene la query
    if !query.is_empty() {
        cmd_str.push_str(&format!(" 2>/dev/null | grep -i '{}'", query));
    }
    cmd_str.push_str(" | head -20");

    match Command::new("sh").arg("-c").arg(&cmd_str).output().await {
        Ok(output) => {
            let results = String::from_utf8_lossy(&output.stdout).to_string();
            let count = results.lines().count();
            ToolResult {
                tool: "search_files".into(), success: true,
                output: if results.is_empty() { "Nessun file trovato.".into() } else { results },
                data: Some(serde_json::json!({"count": count})),
            }
        }
        Err(e) => ToolResult {
            tool: "search_files".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

async fn tool_read_file(call: &ToolCall) -> ToolResult {
    let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return ToolResult { tool: "read_file".into(), success: false, output: "Path non specificato".into(), data: None };
    }

    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let truncated = if content.len() > 5000 {
                format!("{}...\n[troncato a 5000 caratteri]", &content[..5000])
            } else { content };
            ToolResult {
                tool: "read_file".into(), success: true,
                output: truncated, data: None,
            }
        }
        Err(e) => ToolResult {
            tool: "read_file".into(), success: false,
            output: format!("Errore lettura {}: {}", path, e), data: None,
        },
    }
}

async fn tool_system_info(call: &ToolCall) -> ToolResult {
    let what = call.args.get("what").and_then(|v| v.as_str()).unwrap_or("all");

    let mut info = String::new();

    if what == "all" || what == "memory" {
        if let Ok(meminfo) = tokio::fs::read_to_string("/proc/meminfo").await {
            for line in meminfo.lines().take(5) {
                info.push_str(line);
                info.push('\n');
            }
        }
    }

    if what == "all" || what == "disk" {
        if let Ok(output) = Command::new("df").args(["-h", "/"]).output().await {
            info.push_str(&String::from_utf8_lossy(&output.stdout));
        }
    }

    if what == "all" || what == "cpu" {
        if let Ok(loadavg) = tokio::fs::read_to_string("/proc/loadavg").await {
            info.push_str(&format!("Load average: {}\n", loadavg.trim()));
        }
        if let Ok(output) = Command::new("nproc").output().await {
            info.push_str(&format!("CPU cores: {}\n", String::from_utf8_lossy(&output.stdout).trim()));
        }
    }

    if what == "all" || what == "network" {
        if let Ok(output) = Command::new("ip").args(["addr", "show"]).output().await {
            let out = String::from_utf8_lossy(&output.stdout);
            for line in out.lines() {
                if line.contains("inet ") && !line.contains("127.0.0.1") {
                    info.push_str(&format!("{}\n", line.trim()));
                }
            }
        }
    }

    if what == "all" {
        if let Ok(output) = Command::new("uptime").output().await {
            info.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if let Ok(hostname) = tokio::fs::read_to_string("/etc/hostname").await {
            info.push_str(&format!("Hostname: {}\n", hostname.trim()));
        }
    }

    ToolResult {
        tool: "system_info".into(), success: true,
        output: info, data: None,
    }
}

async fn tool_list_files(call: &ToolCall) -> ToolResult {
    let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let recursive = call.args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);

    let cmd = if recursive {
        format!("find {} -maxdepth 3 -type f | head -50", path)
    } else {
        format!("ls -la {}", path)
    };

    match Command::new("sh").arg("-c").arg(&cmd).output().await {
        Ok(output) => ToolResult {
            tool: "list_files".into(), success: output.status.success(),
            output: String::from_utf8_lossy(&output.stdout).to_string(),
            data: None,
        },
        Err(e) => ToolResult {
            tool: "list_files".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

/// Ricerca semantica tramite agent-fs. Se agent-fs non è disponibile,
/// fallback alla ricerca per nome (find + grep sul contenuto).
async fn tool_semantic_search(call: &ToolCall) -> ToolResult {
    let query = call.args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let path = call.args.get("path").and_then(|v| v.as_str());
    let file_type = call.args.get("type").and_then(|v| v.as_str());

    if query.is_empty() {
        return ToolResult {
            tool: "semantic_search".into(), success: false,
            output: "Query di ricerca vuota".into(), data: None,
        };
    }

    // Percorso socket agent-fs — /tmp per macOS, /run per Linux
    #[cfg(target_os = "macos")]
    let fs_socket = "/tmp/agentd-fs.sock";
    #[cfg(not(target_os = "macos"))]
    let fs_socket = "/run/agentd-fs.sock";

    // Prova a connettersi ad agent-fs tramite socket Unix
    if std::path::Path::new(fs_socket).exists() {
        debug!(query = query, socket = fs_socket, "Ricerca semantica via agent-fs");

        // Costruisci la richiesta JSON-RPC per agent-fs
        let search_req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "fs.search",
            "params": {
                "type": "fs.search",
                "query": query,
                "file_type": file_type,
                "folder": path,
                "max_results": 10
            },
            "id": 1
        });

        // Invio sincrono-async tramite UnixStream di tokio
        match tokio::net::UnixStream::connect(fs_socket).await {
            Ok(stream) => {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                let (reader, mut writer) = stream.into_split();

                let req_json = serde_json::to_string(&search_req).unwrap_or_default();
                if writer.write_all(format!("{}\n", req_json).as_bytes()).await.is_ok() {
                    let mut buf_reader = BufReader::new(reader);
                    let mut response_line = String::new();
                    if buf_reader.read_line(&mut response_line).await.is_ok() && !response_line.is_empty() {
                        // Parsa la risposta JSON-RPC
                        if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&response_line) {
                            if let Some(result) = resp.get("result") {
                                let results = result.get("results")
                                    .and_then(|r| r.as_array())
                                    .map(|arr| {
                                        arr.iter().filter_map(|item| {
                                            let p = item.get("path")?.as_str()?;
                                            let snippet = item.get("snippet").and_then(|s| s.as_str()).unwrap_or("");
                                            let score = item.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                                            Some(format!("[{:.0}%] {} — {}", score * 100.0, p, snippet))
                                        }).collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();

                                let output = if results.is_empty() {
                                    "Nessun risultato dalla ricerca semantica.".to_string()
                                } else {
                                    results.join("\n")
                                };

                                return ToolResult {
                                    tool: "semantic_search".into(),
                                    success: true,
                                    output,
                                    data: Some(result.clone()),
                                };
                            }
                        }
                    }
                }
                warn!("Risposta agent-fs non valida — fallback a ricerca locale");
            }
            Err(e) => {
                warn!(error = %e, "Connessione agent-fs fallita — fallback a ricerca locale");
            }
        }
    } else {
        debug!(socket = fs_socket, "agent-fs non disponibile — fallback a ricerca locale");
    }

    // Fallback: ricerca locale con grep nei contenuti dei file
    let search_path = path.unwrap_or("/home");
    let mut cmd_str = format!("find {} -maxdepth 5 -type f", search_path);

    // Filtra per tipo se specificato
    if let Some(ft) = file_type {
        cmd_str.push_str(&format!(" -name '*.{}'", ft));
    } else {
        // Cerca solo file di testo
        cmd_str.push_str(" \\( -name '*.txt' -o -name '*.md' -o -name '*.rs' -o -name '*.py' -o -name '*.js' -o -name '*.ts' -o -name '*.json' -o -name '*.yaml' -o -name '*.toml' -o -name '*.html' -o -name '*.css' -o -name '*.sh' \\)");
    }

    cmd_str.push_str(&format!(" -exec grep -li '{}' {{}} + 2>/dev/null | head -20", query));

    match Command::new("sh").arg("-c").arg(&cmd_str).output().await {
        Ok(output) => {
            let results = String::from_utf8_lossy(&output.stdout).to_string();
            let count = results.lines().count();
            ToolResult {
                tool: "semantic_search".into(), success: true,
                output: if results.is_empty() {
                    format!("Nessun file trovato con contenuto '{}' (ricerca locale — agent-fs non attivo).", query)
                } else {
                    format!("(ricerca locale — agent-fs non attivo)\n{}", results)
                },
                data: Some(serde_json::json!({"count": count, "fallback": true})),
            }
        }
        Err(e) => ToolResult {
            tool: "semantic_search".into(), success: false,
            output: format!("Errore ricerca: {}", e), data: None,
        },
    }
}

// ============================================================
// Tool agent-fs — gestione servizio filesystem semantico
// ============================================================

/// Avvia agent-fs come processo figlio in background.
/// Lancia il binario compilato oppure tramite `cargo run -p agent-fs`.
async fn tool_start_agent_fs(_call: &ToolCall) -> ToolResult {
    // Percorso socket agent-fs
    #[cfg(target_os = "macos")]
    let fs_socket = "/tmp/agentd-fs.sock";
    #[cfg(not(target_os = "macos"))]
    let fs_socket = "/run/agentd-fs.sock";

    // Controlla se agent-fs è già in esecuzione
    if std::path::Path::new(fs_socket).exists() {
        // Verifica che il socket sia attivo provando a connettersi
        if tokio::net::UnixStream::connect(fs_socket).await.is_ok() {
            return ToolResult {
                tool: "start_agent_fs".into(),
                success: true,
                output: "agent-fs è già in esecuzione.".into(),
                data: Some(serde_json::json!({"status": "already_running"})),
            };
        }
        // Socket orfano — rimuovilo
        let _ = std::fs::remove_file(fs_socket);
    }

    // Cerca il binario compilato prima di usare cargo run
    let binary_paths = [
        // Percorso binario in target/debug (sviluppo locale)
        concat!(env!("CARGO_MANIFEST_DIR"), "/../target/debug/agent-fs"),
        // Percorso di installazione sistema
        "/usr/local/bin/agent-fs",
    ];

    let mut child_result = None;
    for bin_path in &binary_paths {
        if std::path::Path::new(bin_path).exists() {
            info!(path = %bin_path, "Avvio agent-fs dal binario");
            child_result = Some(
                Command::new(bin_path)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
            );
            break;
        }
    }

    // Fallback: cargo run (solo in sviluppo)
    if child_result.is_none() {
        info!("Binario agent-fs non trovato — avvio con cargo run");
        child_result = Some(
            Command::new("cargo")
                .args(["run", "-p", "agent-fs"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        );
    }

    match child_result.unwrap() {
        Ok(child) => {
            let pid = child.id().unwrap_or(0);
            info!(pid = pid, "agent-fs avviato come processo figlio");

            // Attendi brevemente che il socket appaia
            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if std::path::Path::new(fs_socket).exists() {
                    return ToolResult {
                        tool: "start_agent_fs".into(),
                        success: true,
                        output: format!("agent-fs avviato (PID: {}). Indicizzazione in corso.", pid),
                        data: Some(serde_json::json!({"pid": pid, "status": "started"})),
                    };
                }
            }

            // Il processo è partito ma il socket non è ancora apparso
            ToolResult {
                tool: "start_agent_fs".into(),
                success: true,
                output: format!(
                    "agent-fs avviato (PID: {}), ma il socket non è ancora pronto. Riprova tra qualche secondo.",
                    pid
                ),
                data: Some(serde_json::json!({"pid": pid, "status": "starting"})),
            }
        }
        Err(e) => {
            warn!(error = %e, "Errore avvio agent-fs");
            ToolResult {
                tool: "start_agent_fs".into(),
                success: false,
                output: format!("Errore avvio agent-fs: {}", e),
                data: None,
            }
        }
    }
}

/// Controlla lo stato di agent-fs: se è attivo, quanti file indicizzati, ecc.
async fn tool_agent_fs_status(_call: &ToolCall) -> ToolResult {
    #[cfg(target_os = "macos")]
    let fs_socket = "/tmp/agentd-fs.sock";
    #[cfg(not(target_os = "macos"))]
    let fs_socket = "/run/agentd-fs.sock";

    if !std::path::Path::new(fs_socket).exists() {
        return ToolResult {
            tool: "agent_fs_status".into(),
            success: true,
            output: "agent-fs non in esecuzione. Usa start_agent_fs per avviarlo.".into(),
            data: Some(serde_json::json!({"running": false})),
        };
    }

    // Invia richiesta di stato via IPC
    let status_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "fs.status",
        "params": { "type": "fs.status" },
        "id": 1
    });

    match tokio::net::UnixStream::connect(fs_socket).await {
        Ok(stream) => {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let (reader, mut writer) = stream.into_split();

            let req_json = serde_json::to_string(&status_req).unwrap_or_default();
            if writer.write_all(format!("{}\n", req_json).as_bytes()).await.is_ok() {
                let mut buf_reader = BufReader::new(reader);
                let mut response_line = String::new();
                if buf_reader.read_line(&mut response_line).await.is_ok() && !response_line.is_empty() {
                    if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&response_line) {
                        if let Some(result) = resp.get("result") {
                            let total = result.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                            let indexed = result.get("indexed_files").and_then(|v| v.as_u64()).unwrap_or(0);
                            let pending = result.get("pending_files").and_then(|v| v.as_u64()).unwrap_or(0);

                            return ToolResult {
                                tool: "agent_fs_status".into(),
                                success: true,
                                output: format!(
                                    "agent-fs attivo. File totali: {}, indicizzati: {}, in attesa: {}",
                                    total, indexed, pending
                                ),
                                data: Some(result.clone()),
                            };
                        }
                    }
                }
            }

            ToolResult {
                tool: "agent_fs_status".into(),
                success: true,
                output: "agent-fs attivo, ma non ha risposto alla richiesta di stato.".into(),
                data: Some(serde_json::json!({"running": true, "status": "no_response"})),
            }
        }
        Err(_) => {
            // Socket esiste ma non risponde — processo probabilmente morto
            let _ = std::fs::remove_file(fs_socket);
            ToolResult {
                tool: "agent_fs_status".into(),
                success: true,
                output: "agent-fs non raggiungibile (socket orfano rimosso). Usa start_agent_fs per riavviarlo.".into(),
                data: Some(serde_json::json!({"running": false, "status": "orphan_cleaned"})),
            }
        }
    }
}

// ============================================================
// Tool Browser — navigazione web
// ============================================================

/// Naviga un URL e restituisce il contenuto testuale della pagina.
/// Usa curl per il fetch e rimuove i tag HTML per estrarre il testo.
async fn tool_browse_url(call: &ToolCall) -> ToolResult {
    let url = call.args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return ToolResult {
            tool: "browse_url".into(), success: false,
            output: "URL non specificato".into(), data: None,
        };
    }

    // Validazione base dell'URL
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolResult {
            tool: "browse_url".into(), success: false,
            output: "URL non valido — deve iniziare con http:// o https://".into(), data: None,
        };
    }

    debug!(url = url, "Navigazione URL");

    // Usa curl con user-agent realistico, segui redirect, timeout 15s
    let cmd = format!(
        "curl -sL --max-time 15 -A 'Mozilla/5.0 (compatible; AgentOS/1.0)' '{}'",
        url.replace('\'', "'\\''") // escape single quotes
    );

    match Command::new("sh").arg("-c").arg(&cmd).output().await {
        Ok(output) => {
            if !output.status.success() {
                return ToolResult {
                    tool: "browse_url".into(), success: false,
                    output: format!(
                        "Errore fetch URL: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                    data: None,
                };
            }

            let html = String::from_utf8_lossy(&output.stdout).to_string();

            // Rimuovi tag HTML e estrai testo leggibile
            let text = strip_html_tags(&html);

            // Tronca a 5000 caratteri
            let truncated = if text.len() > 5000 {
                format!("{}...\n[troncato a 5000 caratteri]", &text[..5000])
            } else {
                text.clone()
            };

            ToolResult {
                tool: "browse_url".into(), success: true,
                output: truncated,
                data: Some(serde_json::json!({
                    "url": url,
                    "length": text.len(),
                    "truncated": text.len() > 5000,
                })),
            }
        }
        Err(e) => ToolResult {
            tool: "browse_url".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

/// Cerca sul web usando DuckDuckGo HTML (nessuna API key necessaria).
/// Restituisce titoli, URL e snippet dei risultati.
async fn tool_web_search(call: &ToolCall) -> ToolResult {
    let query = call.args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let max_results = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    if query.is_empty() {
        return ToolResult {
            tool: "web_search".into(), success: false,
            output: "Query di ricerca vuota".into(), data: None,
        };
    }

    debug!(query = query, "Ricerca web");

    // Usa DuckDuckGo HTML lite (nessuna API key, nessun JavaScript)
    let encoded_query = query.replace(' ', "+").replace('\'', "%27");
    let search_url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        encoded_query
    );

    let cmd = format!(
        "curl -sL --max-time 15 -A 'Mozilla/5.0 (compatible; AgentOS/1.0)' '{}'",
        search_url
    );

    match Command::new("sh").arg("-c").arg(&cmd).output().await {
        Ok(output) => {
            if !output.status.success() {
                return ToolResult {
                    tool: "web_search".into(), success: false,
                    output: format!(
                        "Errore ricerca web: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                    data: None,
                };
            }

            let html = String::from_utf8_lossy(&output.stdout).to_string();

            // Parsa i risultati da DuckDuckGo HTML
            let results = parse_duckduckgo_results(&html, max_results);

            if results.is_empty() {
                return ToolResult {
                    tool: "web_search".into(), success: true,
                    output: format!("Nessun risultato trovato per '{}'.", query),
                    data: Some(serde_json::json!({"query": query, "count": 0})),
                };
            }

            // Formatta i risultati in testo leggibile
            let formatted: Vec<String> = results.iter().enumerate().map(|(i, r)| {
                format!(
                    "{}. {}\n   URL: {}\n   {}",
                    i + 1, r.title, r.url, r.snippet
                )
            }).collect();

            let result_json: Vec<serde_json::Value> = results.iter().map(|r| {
                serde_json::json!({
                    "title": r.title,
                    "url": r.url,
                    "snippet": r.snippet,
                })
            }).collect();

            ToolResult {
                tool: "web_search".into(), success: true,
                output: formatted.join("\n\n"),
                data: Some(serde_json::json!({
                    "query": query,
                    "count": results.len(),
                    "results": result_json,
                })),
            }
        }
        Err(e) => ToolResult {
            tool: "web_search".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

/// Risultato di una ricerca web.
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parsa i risultati dalla pagina HTML di DuckDuckGo.
/// DuckDuckGo HTML ha una struttura semplice con class="result" per ogni risultato.
fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // I risultati DuckDuckGo HTML sono in blocchi con class="result__a" per il link
    // e class="result__snippet" per lo snippet
    for result_block in html.split("class=\"result__a\"").skip(1) {
        if results.len() >= max_results {
            break;
        }

        // Estrai l'URL dal href
        let url = extract_href(result_block).unwrap_or_default();
        // DuckDuckGo wrappa gli URL in un redirect — estrai l'URL reale
        let real_url = extract_ddg_url(&url);

        // Estrai il titolo (testo dentro il tag <a>)
        let title = extract_tag_text(result_block)
            .unwrap_or_else(|| "Senza titolo".to_string());

        // Estrai lo snippet
        let snippet = if let Some(snippet_start) = result_block.find("class=\"result__snippet\"") {
            let after = &result_block[snippet_start..];
            // Trova il > che chiude il tag
            if let Some(gt) = after.find('>') {
                let content = &after[gt + 1..];
                if let Some(end) = content.find("</") {
                    strip_html_tags(&content[..end]).trim().to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !real_url.is_empty() {
            results.push(SearchResult {
                title: title.trim().to_string(),
                url: real_url,
                snippet,
            });
        }
    }

    results
}

/// Estrai l'href dal primo attributo href=" trovato nel testo.
fn extract_href(text: &str) -> Option<String> {
    let href_start = text.find("href=\"")?;
    let after_href = &text[href_start + 6..];
    let end = after_href.find('"')?;
    Some(after_href[..end].to_string())
}

/// Estrai il testo dentro il primo tag (dopo il > iniziale fino al primo <).
fn extract_tag_text(text: &str) -> Option<String> {
    let gt = text.find('>')?;
    let content = &text[gt + 1..];
    let lt = content.find('<')?;
    let raw = &content[..lt];
    Some(strip_html_tags(raw).trim().to_string())
}

/// Estrai l'URL reale dal redirect DuckDuckGo.
/// I link DDG sono tipo //duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&...
fn extract_ddg_url(url: &str) -> String {
    if let Some(uddg_start) = url.find("uddg=") {
        let encoded = &url[uddg_start + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        let encoded_url = &encoded[..end];
        // Decodifica URL encoding base (%XX)
        url_decode(encoded_url)
    } else if url.starts_with("http") {
        url.to_string()
    } else if url.starts_with("//") {
        format!("https:{}", url)
    } else {
        url.to_string()
    }
}

/// Decodifica base URL encoding (%XX → carattere).
fn url_decode(input: &str) -> String {
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

/// Rimuove i tag HTML e restituisce solo il testo leggibile.
/// Gestisce anche entità HTML comuni e normalizza gli spazi.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if in_script {
            // Cerca </script>
            if i + 9 <= lower_chars.len() {
                let slice: String = lower_chars[i..i + 9].iter().collect();
                if slice == "</script>" {
                    in_script = false;
                    i += 9;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        if in_style {
            // Cerca </style>
            if i + 8 <= lower_chars.len() {
                let slice: String = lower_chars[i..i + 8].iter().collect();
                if slice == "</style>" {
                    in_style = false;
                    i += 8;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        if chars[i] == '<' {
            // Controlla se è <script o <style
            if i + 7 <= lower_chars.len() {
                let slice: String = lower_chars[i..i + 7].iter().collect();
                if slice == "<script" {
                    in_script = true;
                    i += 1;
                    continue;
                }
            }
            if i + 6 <= lower_chars.len() {
                let slice: String = lower_chars[i..i + 6].iter().collect();
                if slice == "<style" {
                    in_style = true;
                    i += 1;
                    continue;
                }
            }

            in_tag = true;

            // Aggiungi newline per tag blocco
            if i + 3 <= lower_chars.len() {
                let tag_start: String = lower_chars[i..std::cmp::min(i + 5, lower_chars.len())].iter().collect();
                if tag_start.starts_with("<br") || tag_start.starts_with("<p>") || tag_start.starts_with("<p ")
                    || tag_start.starts_with("<div") || tag_start.starts_with("<li")
                    || tag_start.starts_with("<h1") || tag_start.starts_with("<h2")
                    || tag_start.starts_with("<h3") || tag_start.starts_with("<tr")
                {
                    if !result.ends_with('\n') {
                        result.push('\n');
                    }
                    last_was_space = true;
                }
            }

            i += 1;
            continue;
        }

        if chars[i] == '>' {
            in_tag = false;
            i += 1;
            continue;
        }

        if !in_tag {
            // Gestisci entità HTML
            if chars[i] == '&' {
                if i + 4 <= chars.len() {
                    let entity: String = chars[i..std::cmp::min(i + 6, chars.len())].iter().collect();
                    if entity.starts_with("&amp;") {
                        result.push('&');
                        i += 5;
                        last_was_space = false;
                        continue;
                    } else if entity.starts_with("&lt;") {
                        result.push('<');
                        i += 4;
                        last_was_space = false;
                        continue;
                    } else if entity.starts_with("&gt;") {
                        result.push('>');
                        i += 4;
                        last_was_space = false;
                        continue;
                    } else if entity.starts_with("&quot") {
                        result.push('"');
                        i += 6;
                        last_was_space = false;
                        continue;
                    } else if entity.starts_with("&nbsp") {
                        result.push(' ');
                        i += 6;
                        last_was_space = true;
                        continue;
                    }
                }
            }

            let c = chars[i];
            if c.is_whitespace() {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            } else {
                result.push(c);
                last_was_space = false;
            }
        }

        i += 1;
    }

    // Rimuovi righe vuote consecutive
    let mut cleaned = String::new();
    let mut empty_line_count = 0;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            empty_line_count += 1;
            if empty_line_count <= 1 {
                cleaned.push('\n');
            }
        } else {
            empty_line_count = 0;
            cleaned.push_str(trimmed);
            cleaned.push('\n');
        }
    }

    cleaned.trim().to_string()
}

// ============================================================
// Tool MCP — proxy per tool di server MCP esterni
// ============================================================

/// Proxy una chiamata tool verso un server MCP.
async fn tool_mcp_proxy(call: &ToolCall, mcp: &McpClient) -> ToolResult {
    debug!(tool = %call.tool, "Proxy chiamata MCP");

    match mcp.call_tool(&call.tool, &call.args).await {
        Ok(result) => {
            // Estrai il testo dalla risposta MCP
            // Il formato standard MCP è { "content": [{ "type": "text", "text": "..." }] }
            let output = if let Some(content) = result.get("content") {
                if let Some(arr) = content.as_array() {
                    arr.iter()
                        .filter_map(|item| {
                            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    content.to_string()
                }
            } else {
                // Fallback: serializza tutto il risultato
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            };

            // Verifica se c'è un flag isError nella risposta
            let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);

            ToolResult {
                tool: call.tool.clone(),
                success: !is_error,
                output,
                data: Some(result),
            }
        }
        Err(e) => ToolResult {
            tool: call.tool.clone(),
            success: false,
            output: format!("Errore MCP: {}", e),
            data: None,
        },
    }
}

async fn tool_check_installed(call: &ToolCall) -> ToolResult {
    let program = call.args.get("program").and_then(|v| v.as_str()).unwrap_or("");
    match Command::new("which").arg(program).output().await {
        Ok(output) => {
            let installed = output.status.success();
            ToolResult {
                tool: "check_installed".into(), success: true,
                output: if installed {
                    format!("{} è installato: {}", program, String::from_utf8_lossy(&output.stdout).trim())
                } else {
                    format!("{} NON è installato", program)
                },
                data: Some(serde_json::json!({"installed": installed, "program": program})),
            }
        }
        Err(e) => ToolResult {
            tool: "check_installed".into(), success: false,
            output: format!("Errore: {}", e), data: None,
        },
    }
}

// ============================================================
// Tool Voice — registrazione e trascrizione vocale
// ============================================================

/// Registra audio dal microfono per N secondi e trascrive con Whisper.
/// Su macOS usa `rec` (SoX) per la registrazione.
async fn tool_voice_listen(call: &ToolCall) -> ToolResult {
    let duration = call.args.get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u32;

    // Limita la durata a un range ragionevole (1-30 secondi)
    let duration = duration.clamp(1, 30);

    info!(duration = duration, "Avvio registrazione vocale");

    // Rileva automaticamente il backend Whisper disponibile
    let backend = VoiceInput::detect_backend().await;
    let voice = VoiceInput::new(backend);

    match voice.listen_and_transcribe(duration).await {
        Ok(text) => {
            if text.is_empty() {
                ToolResult {
                    tool: "voice_listen".into(),
                    success: true,
                    output: "(nessun testo rilevato nella registrazione)".into(),
                    data: Some(serde_json::json!({"duration": duration, "text": ""})),
                }
            } else {
                ToolResult {
                    tool: "voice_listen".into(),
                    success: true,
                    output: text.clone(),
                    data: Some(serde_json::json!({"duration": duration, "text": text})),
                }
            }
        }
        Err(e) => ToolResult {
            tool: "voice_listen".into(),
            success: false,
            output: format!("Errore registrazione/trascrizione: {}", e),
            data: None,
        },
    }
}

/// Sintetizza il testo in voce usando macOS `say` (TTS).
async fn tool_voice_speak(call: &ToolCall) -> ToolResult {
    let text = call.args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return ToolResult {
            tool: "voice_speak".into(),
            success: false,
            output: "Testo non specificato per la sintesi vocale".into(),
            data: None,
        };
    }

    match VoiceInput::speak(text).await {
        Ok(()) => ToolResult {
            tool: "voice_speak".into(),
            success: true,
            output: format!("Testo pronunciato: \"{}\"", text),
            data: None,
        },
        Err(e) => ToolResult {
            tool: "voice_speak".into(),
            success: false,
            output: format!("Errore sintesi vocale: {}", e),
            data: None,
        },
    }
}

// ============================================================
// Tool Email e Calendario — connettori Google/Microsoft
// ============================================================

/// Carica il connettore email appropriato dalla configurazione.
/// Restituisce (provider_name, Box<dyn EmailConnector>) oppure errore.
async fn get_email_connector() -> Result<(String, Box<dyn crate::connectors::EmailConnector>), String> {
    // Carica la config per sapere quale connettore usare
    let config = load_connectors_config();

    // Prova Google
    if let Some(gc) = &config.google {
        let connector = crate::connectors::google::GoogleConnector::new(&gc.client_id, &gc.client_secret);
        if connector.is_authenticated().await {
            return Ok(("Google".into(), Box::new(connector)));
        }
    }

    // Prova Microsoft
    if let Some(mc) = &config.microsoft {
        let connector = crate::connectors::outlook::OutlookConnector::new(&mc.client_id, &mc.tenant_id);
        if connector.is_authenticated().await {
            return Ok(("Outlook".into(), Box::new(connector)));
        }
    }

    Err("Nessun connettore email configurato. Usa /connect google o /connect outlook per autenticarti.".into())
}

/// Carica il connettore calendario appropriato dalla configurazione.
async fn get_calendar_connector() -> Result<(String, Box<dyn crate::connectors::CalendarConnector>), String> {
    let config = load_connectors_config();

    // Prova Google
    if let Some(gc) = &config.google {
        let connector = crate::connectors::google::GoogleConnector::new(&gc.client_id, &gc.client_secret);
        if connector.is_authenticated().await {
            return Ok(("Google".into(), Box::new(connector)));
        }
    }

    // Prova Microsoft
    if let Some(mc) = &config.microsoft {
        let connector = crate::connectors::outlook::OutlookConnector::new(&mc.client_id, &mc.tenant_id);
        if connector.is_authenticated().await {
            return Ok(("Outlook".into(), Box::new(connector)));
        }
    }

    Err("Nessun connettore calendario configurato. Usa /connect google o /connect outlook per autenticarti.".into())
}

/// Carica la sezione connectors dalla config (best-effort).
fn load_connectors_config() -> agentos_common::config::ConnectorsConfig {
    // Prova i percorsi noti
    for path in &["/etc/agentos/config.yaml", "config.yaml", "../config.yaml"] {
        if let Ok(config) = agentos_common::config::AgentOsConfig::from_file_with_env(path) {
            return config.connectors;
        }
    }
    agentos_common::config::ConnectorsConfig::default()
}

/// Tool: elenca le email recenti.
async fn tool_list_emails(call: &ToolCall) -> ToolResult {
    let max_results = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10) as u32;

    let (provider, connector) = match get_email_connector().await {
        Ok(c) => c,
        Err(msg) => return ToolResult {
            tool: "list_emails".into(), success: false, output: msg, data: None,
        },
    };

    debug!(provider = %provider, max = max_results, "Recupero email recenti");

    match connector.list_emails(max_results).await {
        Ok(emails) => {
            if emails.is_empty() {
                return ToolResult {
                    tool: "list_emails".into(), success: true,
                    output: format!("Nessuna email trovata ({})", provider), data: None,
                };
            }

            let unread = emails.iter().filter(|e| e.unread).count();
            let mut lines = vec![format!("{} email ({} non lette) — {}", emails.len(), unread, provider)];
            for email in &emails {
                let marker = if email.unread { "* " } else { "  " };
                let date = email.received_at.format("%d/%m %H:%M");
                lines.push(format!(
                    "{}[{}] {} — {} | {}",
                    marker, email.id, date, email.from, email.subject,
                ));
            }

            let data = serde_json::to_value(&emails).ok();
            ToolResult {
                tool: "list_emails".into(), success: true,
                output: lines.join("\n"), data,
            }
        }
        Err(e) => ToolResult {
            tool: "list_emails".into(), success: false,
            output: format!("Errore lettura email: {}", e), data: None,
        },
    }
}

/// Tool: legge una singola email per ID.
async fn tool_read_email(call: &ToolCall) -> ToolResult {
    let email_id = call.args.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if email_id.is_empty() {
        return ToolResult {
            tool: "read_email".into(), success: false,
            output: "ID email non specificato".into(), data: None,
        };
    }

    let (provider, connector) = match get_email_connector().await {
        Ok(c) => c,
        Err(msg) => return ToolResult {
            tool: "read_email".into(), success: false, output: msg, data: None,
        },
    };

    debug!(provider = %provider, id = email_id, "Lettura email");

    match connector.read_email(email_id).await {
        Ok(email) => {
            let output = format!(
                "Da: {}\nA: {}\nOggetto: {}\nData: {}\n\n{}",
                email.from,
                email.to.join(", "),
                email.subject,
                email.received_at.format("%d/%m/%Y %H:%M"),
                email.body_preview,
            );
            let data = serde_json::to_value(&email).ok();
            ToolResult {
                tool: "read_email".into(), success: true,
                output, data,
            }
        }
        Err(e) => ToolResult {
            tool: "read_email".into(), success: false,
            output: format!("Errore lettura email: {}", e), data: None,
        },
    }
}

/// Tool: cerca email per query.
async fn tool_search_emails(call: &ToolCall) -> ToolResult {
    let query = call.args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let max_results = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10) as u32;

    if query.is_empty() {
        return ToolResult {
            tool: "search_emails".into(), success: false,
            output: "Query di ricerca vuota".into(), data: None,
        };
    }

    let (provider, connector) = match get_email_connector().await {
        Ok(c) => c,
        Err(msg) => return ToolResult {
            tool: "search_emails".into(), success: false, output: msg, data: None,
        },
    };

    debug!(provider = %provider, query = query, "Ricerca email");

    match connector.search_emails(query, max_results).await {
        Ok(emails) => {
            if emails.is_empty() {
                return ToolResult {
                    tool: "search_emails".into(), success: true,
                    output: format!("Nessuna email trovata per '{}' ({})", query, provider), data: None,
                };
            }

            let mut lines = vec![format!("{} risultati per '{}' ({})", emails.len(), query, provider)];
            for email in &emails {
                let marker = if email.unread { "* " } else { "  " };
                let date = email.received_at.format("%d/%m %H:%M");
                lines.push(format!(
                    "{}[{}] {} — {} | {}",
                    marker, email.id, date, email.from, email.subject,
                ));
            }

            let data = serde_json::to_value(&emails).ok();
            ToolResult {
                tool: "search_emails".into(), success: true,
                output: lines.join("\n"), data,
            }
        }
        Err(e) => ToolResult {
            tool: "search_emails".into(), success: false,
            output: format!("Errore ricerca email: {}", e), data: None,
        },
    }
}

/// Tool: elenca gli eventi del calendario.
async fn tool_list_events(call: &ToolCall) -> ToolResult {
    let days = call.args.get("days").and_then(|v| v.as_u64()).unwrap_or(7) as u32;
    let max_results = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10) as u32;

    let (provider, connector) = match get_calendar_connector().await {
        Ok(c) => c,
        Err(msg) => return ToolResult {
            tool: "list_events".into(), success: false, output: msg, data: None,
        },
    };

    debug!(provider = %provider, days = days, "Recupero eventi calendario");

    match connector.list_events(days, max_results).await {
        Ok(events) => {
            if events.is_empty() {
                return ToolResult {
                    tool: "list_events".into(), success: true,
                    output: format!("Nessun evento nei prossimi {} giorni ({})", days, provider),
                    data: None,
                };
            }

            let mut lines = vec![format!("{} eventi nei prossimi {} giorni ({})", events.len(), days, provider)];
            for event in &events {
                let time_str = if event.all_day {
                    event.start.format("%d/%m (tutto il giorno)").to_string()
                } else {
                    format!("{} - {}", event.start.format("%d/%m %H:%M"), event.end.format("%H:%M"))
                };
                let loc = event.location.as_deref().map(|l| format!(" @ {}", l)).unwrap_or_default();
                lines.push(format!("  {} {}{}", time_str, event.title, loc));
            }

            let data = serde_json::to_value(&events).ok();
            ToolResult {
                tool: "list_events".into(), success: true,
                output: lines.join("\n"), data,
            }
        }
        Err(e) => ToolResult {
            tool: "list_events".into(), success: false,
            output: format!("Errore lettura calendario: {}", e), data: None,
        },
    }
}

/// Tool: crea un evento nel calendario.
async fn tool_create_event(call: &ToolCall) -> ToolResult {
    let title = call.args.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let start_str = call.args.get("start").and_then(|v| v.as_str()).unwrap_or("");
    let end_str = call.args.get("end").and_then(|v| v.as_str()).unwrap_or("");

    if title.is_empty() || start_str.is_empty() || end_str.is_empty() {
        return ToolResult {
            tool: "create_event".into(), success: false,
            output: "Parametri mancanti: title, start, end sono obbligatori".into(), data: None,
        };
    }

    // Parsa le date
    let start = match chrono::DateTime::parse_from_rfc3339(start_str) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return ToolResult {
            tool: "create_event".into(), success: false,
            output: format!("Formato data start non valido: {}. Usa formato ISO 8601 (es. 2026-03-20T10:00:00Z)", start_str),
            data: None,
        },
    };
    let end = match chrono::DateTime::parse_from_rfc3339(end_str) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return ToolResult {
            tool: "create_event".into(), success: false,
            output: format!("Formato data end non valido: {}. Usa formato ISO 8601", end_str),
            data: None,
        },
    };

    let location = call.args.get("location").and_then(|v| v.as_str()).map(String::from);
    let description = call.args.get("description").and_then(|v| v.as_str()).map(String::from);

    let (provider, connector) = match get_calendar_connector().await {
        Ok(c) => c,
        Err(msg) => return ToolResult {
            tool: "create_event".into(), success: false, output: msg, data: None,
        },
    };

    debug!(provider = %provider, title = title, "Creazione evento");

    let params = crate::connectors::CreateEventParams {
        title: title.to_string(),
        start,
        end,
        location,
        description,
    };

    match connector.create_event(params).await {
        Ok(event) => {
            let loc = event.location.as_deref().map(|l| format!(" @ {}", l)).unwrap_or_default();
            let output = format!(
                "Evento creato ({}): {} — {} {} {}{}",
                provider,
                event.id,
                event.title,
                event.start.format("%d/%m/%Y %H:%M"),
                event.end.format("- %H:%M"),
                loc,
            );
            let data = serde_json::to_value(&event).ok();
            ToolResult {
                tool: "create_event".into(), success: true,
                output, data,
            }
        }
        Err(e) => ToolResult {
            tool: "create_event".into(), success: false,
            output: format!("Errore creazione evento: {}", e), data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_command_echo() {
        let call = ToolCall {
            tool: "run_command".into(),
            args: serde_json::json!({"command": "echo ciao"}),
        };
        let result = execute_tool(&call).await;
        assert!(result.success);
        assert!(result.output.contains("ciao"));
    }

    #[tokio::test]
    async fn test_system_info() {
        let call = ToolCall {
            tool: "system_info".into(),
            args: serde_json::json!({"what": "all"}),
        };
        let result = execute_tool(&call).await;
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_check_installed_ls() {
        let call = ToolCall {
            tool: "check_installed".into(),
            args: serde_json::json!({"program": "ls"}),
        };
        let result = execute_tool(&call).await;
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let call = ToolCall {
            tool: "nonexistent".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_list_files() {
        let call = ToolCall {
            tool: "list_files".into(),
            args: serde_json::json!({"path": "/tmp"}),
        };
        let result = execute_tool(&call).await;
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_browse_url_empty() {
        let call = ToolCall {
            tool: "browse_url".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("URL non specificato"));
    }

    #[tokio::test]
    async fn test_browse_url_invalid() {
        let call = ToolCall {
            tool: "browse_url".into(),
            args: serde_json::json!({"url": "not-a-url"}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("http"));
    }

    #[tokio::test]
    async fn test_web_search_empty() {
        let call = ToolCall {
            tool: "web_search".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("vuota"));
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(
            strip_html_tags("<p>Ciao <b>mondo</b></p>"),
            "Ciao mondo"
        );
        assert_eq!(
            strip_html_tags("<script>alert('x')</script>Testo"),
            "Testo"
        );
        assert_eq!(
            strip_html_tags("a &amp; b &lt; c"),
            "a & b < c"
        );
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("https%3A%2F%2Fexample.com"), "https://example.com");
        assert_eq!(url_decode("ciao+mondo"), "ciao mondo");
    }

    #[test]
    fn test_extract_ddg_url() {
        let ddg = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        assert_eq!(extract_ddg_url(ddg), "https://example.com/page");

        let direct = "https://example.com/direct";
        assert_eq!(extract_ddg_url(direct), "https://example.com/direct");
    }

    #[test]
    fn test_parse_duckduckgo_empty() {
        let results = parse_duckduckgo_results("<html><body>No results</body></html>", 5);
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_voice_listen_empty_args() {
        // Verifica che il tool gestisca correttamente argomenti mancanti
        // (la durata di default è 5 secondi, ma il tool fallirà se manca il registratore)
        let call = ToolCall {
            tool: "voice_listen".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        // Non verifichiamo success perché dipende dall'hardware, ma non deve fare panic
        assert_eq!(result.tool, "voice_listen");
    }

    #[tokio::test]
    async fn test_voice_speak_empty() {
        let call = ToolCall {
            tool: "voice_speak".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("Testo non specificato"));
    }

    #[tokio::test]
    async fn test_list_emails_no_connector() {
        // Senza connettore configurato, il tool deve restituire un messaggio di errore
        let call = ToolCall {
            tool: "list_emails".into(),
            args: serde_json::json!({"max_results": 5}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("connettore") || result.output.contains("configurato") || result.output.contains("connect"));
    }

    #[tokio::test]
    async fn test_read_email_missing_id() {
        let call = ToolCall {
            tool: "read_email".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("ID"));
    }

    #[tokio::test]
    async fn test_search_emails_empty_query() {
        let call = ToolCall {
            tool: "search_emails".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("vuota"));
    }

    #[tokio::test]
    async fn test_list_events_no_connector() {
        let call = ToolCall {
            tool: "list_events".into(),
            args: serde_json::json!({"days": 7}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("connettore") || result.output.contains("configurato") || result.output.contains("connect"));
    }

    #[tokio::test]
    async fn test_create_event_missing_params() {
        let call = ToolCall {
            tool: "create_event".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("obbligatori") || result.output.contains("mancanti"));
    }

    #[tokio::test]
    async fn test_connect_service_empty_provider() {
        let call = ToolCall {
            tool: "connect_service".into(),
            args: serde_json::json!({}),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("Provider") || result.output.contains("provider"));
    }

    #[tokio::test]
    async fn test_create_event_invalid_date() {
        let call = ToolCall {
            tool: "create_event".into(),
            args: serde_json::json!({
                "title": "Test",
                "start": "not-a-date",
                "end": "2026-03-20T11:00:00Z"
            }),
        };
        let result = execute_tool(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("data") || result.output.contains("ISO"));
    }
}

// ============================================================
// Auto-sviluppo — l'agente crea i propri tool e app
// ============================================================

/// Crea un nuovo tool custom (script Python/Bash/Node).
async fn tool_create_tool(call: &ToolCall) -> ToolResult {
    let name = call.args.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = call.args.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let language = call.args.get("language").and_then(|v| v.as_str()).unwrap_or("python");
    let code = call.args.get("code").and_then(|v| v.as_str()).unwrap_or("");

    if name.is_empty() || code.is_empty() {
        return ToolResult { tool: "create_tool".into(), success: false, output: "Nome e codice sono obbligatori".into(), data: None };
    }

    let parameters: Vec<ToolParam> = call.args.get("parameters")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut forge = TOOL_FORGE.lock().await;
    match forge.create_tool(name, description, language, code, parameters) {
        Ok(msg) => {
            info!(name = name, "Tool custom creato dall'agente");
            ToolResult { tool: "create_tool".into(), success: true, output: msg, data: Some(serde_json::json!({"tool_name": format!("custom_{}", name)})) }
        }
        Err(e) => ToolResult { tool: "create_tool".into(), success: false, output: e, data: None }
    }
}

/// Crea un'app completa con più file.
async fn tool_create_app(call: &ToolCall) -> ToolResult {
    let name = call.args.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = call.args.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let language = call.args.get("language").and_then(|v| v.as_str()).unwrap_or("python");
    let files = call.args.get("files").and_then(|v| v.as_object());

    if name.is_empty() {
        return ToolResult { tool: "create_app".into(), success: false, output: "Nome obbligatorio".into(), data: None };
    }

    let files_map: std::collections::HashMap<String, String> = match files {
        Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect(),
        None => return ToolResult { tool: "create_app".into(), success: false, output: "Campo 'files' obbligatorio: {\"main.py\": \"codice...\"}".into(), data: None },
    };

    let mut forge = TOOL_FORGE.lock().await;
    match forge.create_app(name, description, language, &files_map) {
        Ok(msg) => ToolResult { tool: "create_app".into(), success: true, output: msg, data: None },
        Err(e) => ToolResult { tool: "create_app".into(), success: false, output: e, data: None },
    }
}

/// Modifica un file — sovrascrittura completa o find/replace.
async fn tool_edit_file(call: &ToolCall) -> ToolResult {
    let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return ToolResult { tool: "edit_file".into(), success: false, output: "Path obbligatorio".into(), data: None };
    }

    // Modalità 1: find/replace
    if let (Some(find), Some(replace)) = (call.args.get("find").and_then(|v| v.as_str()), call.args.get("replace").and_then(|v| v.as_str())) {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let count = content.matches(find).count();
                if count == 0 {
                    return ToolResult { tool: "edit_file".into(), success: false, output: format!("Testo '{}' non trovato nel file", find), data: None };
                }
                let new_content = content.replace(find, replace);
                match std::fs::write(path, &new_content) {
                    Ok(_) => ToolResult { tool: "edit_file".into(), success: true, output: format!("{} occorrenze sostituite", count), data: None },
                    Err(e) => ToolResult { tool: "edit_file".into(), success: false, output: format!("Errore scrittura: {}", e), data: None },
                }
            }
            Err(e) => ToolResult { tool: "edit_file".into(), success: false, output: format!("Errore lettura: {}", e), data: None },
        }
    }
    // Modalità 2: sovrascrittura completa
    else if let Some(content) = call.args.get("content").and_then(|v| v.as_str()) {
        match std::fs::write(path, content) {
            Ok(_) => ToolResult { tool: "edit_file".into(), success: true, output: format!("File '{}' scritto ({} bytes)", path, content.len()), data: None },
            Err(e) => ToolResult { tool: "edit_file".into(), success: false, output: format!("Errore: {}", e), data: None },
        }
    }
    // Modalità 3: append
    else if let Some(append) = call.args.get("append").and_then(|v| v.as_str()) {
        use std::io::Write;
        match std::fs::OpenOptions::new().append(true).create(true).open(path) {
            Ok(mut f) => {
                writeln!(f, "{}", append).ok();
                ToolResult { tool: "edit_file".into(), success: true, output: format!("Testo aggiunto a '{}'", path), data: None }
            }
            Err(e) => ToolResult { tool: "edit_file".into(), success: false, output: format!("Errore: {}", e), data: None },
        }
    } else {
        ToolResult { tool: "edit_file".into(), success: false, output: "Specifica 'content' (sovrascrittura), 'find'+'replace', o 'append'".into(), data: None }
    }
}

/// Lista i tool custom creati dall'agente.
async fn tool_list_custom_tools(_call: &ToolCall) -> ToolResult {
    let forge = TOOL_FORGE.lock().await;
    let tools = forge.list_tools();
    let apps = forge.list_apps();

    let mut output = String::new();
    if tools.is_empty() && apps.is_empty() {
        output.push_str("Nessun tool o app custom creato. Usa create_tool per crearne uno.");
    } else {
        if !tools.is_empty() {
            output.push_str("Tool custom:\n");
            for t in &tools {
                output.push_str(&format!("  • custom_{} ({}) — {} [usato {} volte]\n", t.name, t.language, t.description, t.use_count));
            }
        }
        if !apps.is_empty() {
            output.push_str("App create:\n");
            for a in &apps {
                output.push_str(&format!("  • {} ({}) — {}\n", a.name, a.language, a.description));
            }
        }
    }

    ToolResult { tool: "list_custom_tools".into(), success: true, output, data: None }
}

// ============================================================
// Tool OAuth — connessione servizi zero-config
// ============================================================

/// Connette un servizio (Gmail, Outlook) tramite flusso OAuth nel browser.
/// L'utente dice "connetti gmail" e l'agente gestisce tutto automaticamente.
async fn tool_connect_service(call: &ToolCall) -> ToolResult {
    let provider = call.args.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    if provider.is_empty() {
        return ToolResult {
            tool: "connect_service".into(),
            success: false,
            output: "Provider non specificato. Usa: gmail, outlook".into(),
            data: None,
        };
    }

    info!(provider = provider, "Avvio flusso OAuth zero-config");

    let oauth = crate::oauth::OAuthFlow::new();

    match oauth.start_flow(provider).await {
        Ok(_tokens) => {
            let provider_name = match provider.to_lowercase().as_str() {
                "gmail" | "google" => "Google (Gmail + Calendar)",
                "outlook" | "microsoft" => "Microsoft (Outlook + Calendar)",
                _ => provider,
            };

            ToolResult {
                tool: "connect_service".into(),
                success: true,
                output: format!(
                    "Connessione a {} completata! I token sono stati salvati.\n\
                     Ora puoi usare i comandi email e calendario.",
                    provider_name
                ),
                data: Some(serde_json::json!({
                    "provider": provider,
                    "status": "connected"
                })),
            }
        }
        Err(e) => {
            warn!(provider = provider, error = %e, "Errore flusso OAuth");
            ToolResult {
                tool: "connect_service".into(),
                success: false,
                output: format!("Errore connessione {}: {}", provider, e),
                data: Some(serde_json::json!({
                    "provider": provider,
                    "status": "error"
                })),
            }
        }
    }
}

/// Restituisce la descrizione dei tool custom per il prompt LLM.
pub fn get_custom_tools_description() -> String {
    let forge = match TOOL_FORGE.try_lock() { Ok(f) => f, Err(_) => return String::new() };
    forge.tools_description()
}

// ============================================================
// Sub-agenti paralleli
// ============================================================

/// Spawna un singolo sub-agente con un obiettivo.
async fn tool_spawn_agent(call: &ToolCall) -> ToolResult {
    let objective = call.args.get("objective").and_then(|v| v.as_str()).unwrap_or("");
    if objective.is_empty() {
        return ToolResult { tool: "spawn_agent".into(), success: false, output: "Obiettivo obbligatorio".into(), data: None };
    }

    let mgr = {
        let guard = AGENT_MANAGER.lock().await;
        match guard.as_ref() {
            Some(m) => m.clone(),
            None => return ToolResult { tool: "spawn_agent".into(), success: false, output: "Agent manager non inizializzato".into(), data: None },
        }
    }; // guard rilasciato qui

    let id = mgr.spawn_agent(objective, 1).await;
    let results = mgr.wait_all(&[id.clone()]).await;
    let result_text = results.first().map(|(_, r)| r.clone()).unwrap_or_default();

    ToolResult {
        tool: "spawn_agent".into(),
        success: true,
        output: format!("Sub-agente [{}] completato:\n{}", id, result_text),
        data: Some(serde_json::json!({"agent_id": id})),
    }
}

/// Spawna N sub-agenti in parallelo e attende tutti i risultati.
async fn tool_spawn_parallel(call: &ToolCall) -> ToolResult {
    let objectives: Vec<String> = call.args.get("objectives")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if objectives.is_empty() {
        return ToolResult { tool: "spawn_parallel".into(), success: false, output: "Lista 'objectives' vuota".into(), data: None };
    }

    let mgr = {
        let guard = AGENT_MANAGER.lock().await;
        match guard.as_ref() {
            Some(m) => m.clone(),
            None => return ToolResult { tool: "spawn_parallel".into(), success: false, output: "Agent manager non inizializzato".into(), data: None },
        }
    };

    info!(count = objectives.len(), "Spawn di {} sub-agenti in parallelo", objectives.len());
    let results = mgr.spawn_parallel(&objectives, 1).await;

    let mut output = format!("Risultati da {} sub-agenti:\n\n", results.len());
    for (i, (id, result)) in results.iter().enumerate() {
        output.push_str(&format!("── Agente {} [{}] ──\n{}\n\n", i + 1, id, result));
    }

    ToolResult {
        tool: "spawn_parallel".into(),
        success: true,
        output,
        data: Some(serde_json::json!({"agent_ids": results.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()})),
    }
}

/// Lista i sub-agenti attivi e completati.
async fn tool_list_agents(call: &ToolCall) -> ToolResult {
    let mgr = {
        let guard = AGENT_MANAGER.lock().await;
        match guard.as_ref() {
            Some(m) => m.clone(),
            None => return ToolResult { tool: "list_agents".into(), success: false, output: "Agent manager non inizializzato".into(), data: None },
        }
    };

    let tasks = mgr.list_tasks().await;
    if tasks.is_empty() {
        return ToolResult { tool: "list_agents".into(), success: true, output: "Nessun sub-agente attivo.".into(), data: None };
    }

    let mut output = format!("{} sub-agenti:\n", tasks.len());
    for t in &tasks {
        let status = match t.status {
            crate::agents::AgentStatus::Running => "⟳ in corso",
            crate::agents::AgentStatus::Completed => "✓ completato",
            crate::agents::AgentStatus::Failed => "✗ fallito",
        };
        output.push_str(&format!("  [{}] {} — {} ({} azioni)\n", t.id, status, t.objective, t.actions.len()));
    }

    ToolResult { tool: "list_agents".into(), success: true, output, data: None }
}
