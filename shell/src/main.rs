//! AgentOS CLI — interfaccia terminale interattiva per agentd.
//! Comandi slash: /settings, /provider, /model, /key, /ollama, /help, /voice, /say

use agentos_common::ipc::{JsonRpcRequest, JsonRpcResponse, ShellToAgent};
use agentos_common::types::RiskZone;
use std::io::{self, BufRead, Write};

const CONFIG_PATHS: &[&str] = &["/etc/agentos/config.yaml", "config.yaml", "../config.yaml"];

/// Percorso del file di onboarding — indica che il wizard è stato completato
fn onboarded_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home).join(".agentos").join("onboarded")
}

fn main() {
    // Feature 2: Onboarding wizard al primo avvio
    if !onboarded_path().exists() {
        run_onboarding_wizard();
    }

    let socket_path = if std::path::Path::new("/run/agentd.sock").exists() { "/run/agentd.sock" }
        else if std::path::Path::new("/tmp/agentd.sock").exists() { "/tmp/agentd.sock" }
        else {
            eprintln!("\x1b[31m✗ agentd non trovato. Avvialo con: cargo run -p agentd &\x1b[0m");
            std::process::exit(1);
        };

    println!("\x1b[36m◆ AgentOS\x1b[0m — connesso a agentd");
    println!("  Scrivi qualsiasi cosa. \x1b[2m/help per i comandi. Ctrl+C per uscire.\x1b[0m");
    println!();

    let mut req_id = 1u64;
    let stdin = io::stdin();

    // Briefing al boot
    send_briefing(socket_path, &mut req_id);

    loop {
        print!("\x1b[36m❯\x1b[0m ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if stdin.lock().read_line(&mut input).is_err() || input.is_empty() { break; }
        let text = input.trim();
        if text.is_empty() { continue; }
        if text == "exit" || text == "quit" { break; }

        // Comandi slash
        if text.starts_with('/') {
            handle_slash_command(text, socket_path, &mut req_id);
            continue;
        }

        // Messaggio all'agente con gestione conferma zona gialla (Feature 3)
        send_to_agent_with_confirm(text, socket_path, &mut req_id);
        println!();
    }
}

// ============================================================
// Feature 2: Onboarding Wizard
// ============================================================

/// Wizard di configurazione iniziale mostrato al primo avvio.
/// Guida l'utente nella scelta del provider LLM e salva la configurazione.
fn run_onboarding_wizard() {
    let stdin = io::stdin();

    println!();
    println!("\x1b[36m╔══════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║   Benvenuto in AgentOS!              ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════╝\x1b[0m");
    println!();

    // Passo 1: nome utente
    print!("\x1b[1mCome ti chiami?\x1b[0m ");
    io::stdout().flush().unwrap();
    let mut name = String::new();
    stdin.lock().read_line(&mut name).unwrap();
    let name = name.trim().to_string();
    let display_name = if name.is_empty() { "Utente".to_string() } else { name };

    println!();
    println!("Ciao \x1b[32m{}\x1b[0m!", display_name);
    println!();

    // Passo 2: scelta provider LLM
    println!("\x1b[1mQuale provider LLM vuoi usare?\x1b[0m");
    println!("  \x1b[36m(1)\x1b[0m OpenAI    — GPT-4o, necessita API key");
    println!("  \x1b[36m(2)\x1b[0m Ollama    — LLM locale, gratuito (llama3.2, mistral, ecc.)");
    println!("  \x1b[36m(3)\x1b[0m Claude    — Anthropic Claude, necessita API key");
    println!();
    print!("Scelta [1/2/3]: ");
    io::stdout().flush().unwrap();

    let mut choice = String::new();
    stdin.lock().read_line(&mut choice).unwrap();
    let choice = choice.trim();

    let (provider, mut api_key, mut model) = match choice {
        "1" | "openai" => ("openai".to_string(), String::new(), "gpt-4o".to_string()),
        "3" | "claude" => ("claude".to_string(), String::new(), "claude-sonnet-4-20250514".to_string()),
        _ => ("ollama".to_string(), String::new(), "llama3.2".to_string()),
    };

    println!();

    // Passo 3: API key (se necessaria)
    if provider == "openai" || provider == "claude" {
        let provider_name = if provider == "openai" { "OpenAI" } else { "Claude (Anthropic)" };
        print!("\x1b[1mInserisci la API key {}:\x1b[0m ", provider_name);
        io::stdout().flush().unwrap();
        let mut key_input = String::new();
        stdin.lock().read_line(&mut key_input).unwrap();
        api_key = key_input.trim().to_string();

        if api_key.is_empty() {
            println!("\x1b[33m⚠ Nessuna API key inserita. Potrai aggiungerla dopo con /key\x1b[0m");
        } else {
            println!("\x1b[32m✓ API key salvata\x1b[0m");
        }
        println!();
    }

    // Passo 4: modello Ollama (se scelto)
    if provider == "ollama" {
        print!("\x1b[1mQuale modello Ollama? (default: llama3.2):\x1b[0m ");
        io::stdout().flush().unwrap();
        let mut model_input = String::new();
        stdin.lock().read_line(&mut model_input).unwrap();
        let model_input = model_input.trim();
        if !model_input.is_empty() {
            model = model_input.to_string();
        }

        println!();
        println!("  \x1b[2mSe non hai il modello, usa: /ollama pull {}\x1b[0m", model);
        println!("  \x1b[2mPer modelli cloud Ollama: /ollama login\x1b[0m");
        println!();
    }

    // Passo 5: salva la configurazione
    if let Some(mut config) = load_config() {
        config["llm"]["default_backend"] = serde_json::Value::String(provider.clone());
        config["llm"]["complex_backend"] = serde_json::Value::String(provider.clone());

        match provider.as_str() {
            "openai" => {
                config["openai"]["model"] = serde_json::Value::String(model);
                if !api_key.is_empty() {
                    config["openai"]["api_key"] = serde_json::Value::String(api_key);
                }
            }
            "claude" => {
                config["claude"]["model"] = serde_json::Value::String(model);
                if !api_key.is_empty() {
                    config["claude"]["api_key"] = serde_json::Value::String(api_key);
                }
            }
            "ollama" => {
                config["ollama"]["model"] = serde_json::Value::String(model);
            }
            _ => {}
        }

        if let Err(e) = save_config(&config) {
            eprintln!("\x1b[31m✗ Errore salvataggio config: {}\x1b[0m", e);
        }
    }

    // Crea il file onboarded e la directory ~/.agentos
    let onboarded = onboarded_path();
    if let Some(parent) = onboarded.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Salva il nome utente nel file onboarded
    let _ = std::fs::write(&onboarded, format!("name={}\nprovider={}", display_name, provider));

    println!("\x1b[32m✓ Tutto pronto!\x1b[0m Scrivi \x1b[1m/help\x1b[0m per i comandi disponibili.");
    println!();
}

// ── Comandi slash ──

fn handle_slash_command(cmd: &str, socket_path: &str, req_id: &mut u64) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match command {
        "/help" | "/h" => {
            println!("\x1b[36m── Comandi ──\x1b[0m");
            println!("  \x1b[1m/settings\x1b[0m          — mostra configurazione corrente");
            println!("  \x1b[1m/provider <nome>\x1b[0m   — cambia provider (ollama, openai, claude)");
            println!("  \x1b[1m/model <nome>\x1b[0m      — cambia modello (es. gpt-4o, llama3.2, kimi-k2.5:cloud)");
            println!("  \x1b[1m/key <provider> <key>\x1b[0m — imposta API key (es. /key openai sk-...)");
            println!("  \x1b[1m/ollama url <url>\x1b[0m  — cambia URL Ollama");
            println!("  \x1b[1m/ollama login\x1b[0m      — login a Ollama (per modelli cloud)");
            println!("  \x1b[1m/ollama models\x1b[0m     — lista modelli Ollama disponibili");
            println!("  \x1b[1m/ollama pull <m>\x1b[0m   — scarica un modello Ollama");
            println!("  \x1b[1m/voice [secondi]\x1b[0m   — registra audio e invia trascrizione all'agente");
            println!("  \x1b[1m/say <testo>\x1b[0m       — sintesi vocale (TTS) con macOS `say`");
            println!("  \x1b[1m/restart\x1b[0m           — riavvia agentd con la nuova config");
            println!("  \x1b[1m/briefing\x1b[0m          — mostra il briefing");
            println!("  \x1b[1m/fs start\x1b[0m          — avvia agent-fs (indicizzazione semantica)");
            println!("  \x1b[1m/fs status\x1b[0m         — stato di agent-fs");
            println!("  \x1b[1m/fs search <query>\x1b[0m — ricerca semantica nei file");
            println!("  \x1b[1m/connect google\x1b[0m    — collega account Google (Gmail + Calendar)");
            println!("  \x1b[1m/connect outlook\x1b[0m   — collega account Microsoft (Outlook + Calendar)");
            println!("  \x1b[1m/emails\x1b[0m            — mostra email recenti");
            println!("  \x1b[1m/calendar\x1b[0m          — mostra prossimi eventi calendario");
            println!("\x1b[36m─────────────\x1b[0m");
        }

        "/settings" | "/config" => {
            show_settings();
        }

        "/provider" => {
            if arg.is_empty() {
                println!("\x1b[33mUso: /provider <ollama|openai|claude>\x1b[0m");
            } else {
                set_provider(arg);
            }
        }

        "/model" => {
            if arg.is_empty() {
                println!("\x1b[33mUso: /model <nome_modello>\x1b[0m");
                println!("  Esempi: gpt-4o, llama3.2, kimi-k2.5:cloud, claude-sonnet-4-20250514");
            } else {
                set_model(arg);
            }
        }

        "/key" => {
            let key_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            if key_parts.len() < 2 {
                println!("\x1b[33mUso: /key <openai|claude> <api_key>\x1b[0m");
            } else {
                set_api_key(key_parts[0], key_parts[1]);
            }
        }

        "/ollama" => {
            handle_ollama_command(arg);
        }

        // Feature 1: registrazione vocale e invio trascrizione all'agente
        "/voice" | "/v" => {
            handle_voice_command(arg, socket_path, req_id);
        }

        // Feature 1: sintesi vocale (TTS) con macOS `say`
        "/say" => {
            handle_say_command(arg);
        }

        "/restart" => {
            println!("\x1b[33m⟳ Riavvio agentd...\x1b[0m");
            let _ = std::process::Command::new("pkill").arg("-f").arg("agentd").output();
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = std::process::Command::new("sh").arg("-c")
                .arg("cd /Users/orma/Documents/DevProjectsLocal/LAB/agent-os && cargo run -p agentd &>/tmp/agentd-local.log &")
                .spawn();
            std::thread::sleep(std::time::Duration::from_secs(3));
            if std::path::Path::new(socket_path).exists() {
                println!("\x1b[32m✓ agentd riavviato\x1b[0m");
            } else {
                println!("\x1b[31m✗ agentd non si è riavviato. Controlla /tmp/agentd-local.log\x1b[0m");
            }
        }

        "/briefing" => {
            send_briefing(socket_path, req_id);
        }

        "/fs" => {
            handle_fs_command(arg, socket_path, req_id);
        }

        "/connect" => {
            handle_connect_command(arg, socket_path, req_id);
        }

        "/emails" | "/email" | "/mail" => {
            // Invia come richiesta all'agente per usare il tool list_emails
            send_to_agent_with_confirm("mostrami le email recenti", socket_path, req_id);
        }

        "/calendar" | "/cal" | "/agenda" => {
            // Invia come richiesta all'agente per usare il tool list_events
            send_to_agent_with_confirm("mostrami gli appuntamenti di questa settimana", socket_path, req_id);
        }

        _ => {
            println!("\x1b[33mComando sconosciuto: {}. Scrivi /help\x1b[0m", command);
        }
    }
    println!();
}

// ============================================================
// Comandi connettori — /connect google, /connect outlook
// ============================================================

/// Gestisce il comando /connect per connettere servizi email/calendario.
/// Usa il flusso OAuth zero-config: delega all'agente che apre il browser.
fn handle_connect_command(arg: &str, socket_path: &str, req_id: &mut u64) {
    match arg {
        "google" | "gmail" => {
            println!("\x1b[36m── Connessione Google (Gmail + Calendar) ──\x1b[0m");
            println!("  Si aprirà il browser per l'autorizzazione...");
            println!();
            send_to_agent_with_confirm("connetti gmail", socket_path, req_id);
        }

        "outlook" | "microsoft" => {
            println!("\x1b[36m── Connessione Microsoft (Outlook + Calendar) ──\x1b[0m");
            println!("  Si aprirà il browser per l'autorizzazione...");
            println!();
            send_to_agent_with_confirm("connetti outlook", socket_path, req_id);
        }

        "" => {
            println!("\x1b[33mUso: /connect <google|outlook>\x1b[0m");
            println!("  google   — Gmail + Google Calendar");
            println!("  outlook  — Outlook Mail + Calendar");
            println!();
            println!("  Il flusso apre il browser automaticamente.");
            println!("  Puoi anche scrivere: \x1b[1mconnetti gmail\x1b[0m");
        }

        _ => {
            println!("\x1b[33mProvider sconosciuto: {}. Usa: google, outlook\x1b[0m", arg);
        }
    }
}

// ============================================================
// Feature 1: Comandi vocali — /voice e /say
// ============================================================

/// Registra audio dal microfono, trascrive e invia la trascrizione all'agente.
/// Usa il tool voice_listen lato agentd per la registrazione e trascrizione.
fn handle_voice_command(arg: &str, socket_path: &str, req_id: &mut u64) {
    let duration: u32 = arg.parse().unwrap_or(5);
    let duration = duration.clamp(1, 30);

    println!("\x1b[36m🎤 Registrazione per {} secondi... (parla adesso)\x1b[0m", duration);

    // Invia il comando di registrazione come UserInput con richiesta di voice_listen
    // L'agente interpreterà la richiesta e userà il tool voice_listen
    let voice_text = format!("[VOICE_INPUT duration={}] Registra e trascrivi audio", duration);
    send_to_agent_with_confirm(&voice_text, socket_path, req_id);
}

/// Sintesi vocale con macOS `say`. Esegue direttamente il comando `say`.
fn handle_say_command(arg: &str) {
    if arg.is_empty() {
        println!("\x1b[33mUso: /say <testo da pronunciare>\x1b[0m");
        return;
    }

    println!("\x1b[36m🔊 Pronuncio: \"{}\"\x1b[0m", arg);

    // Prova prima con la voce italiana Alice
    let result = std::process::Command::new("say")
        .args(["-v", "Alice", arg])
        .status();

    match result {
        Ok(s) if s.success() => {
            println!("\x1b[32m✓ Fatto\x1b[0m");
        }
        _ => {
            // Fallback senza specificare la voce
            let fallback = std::process::Command::new("say")
                .arg(arg)
                .status();
            match fallback {
                Ok(s) if s.success() => println!("\x1b[32m✓ Fatto\x1b[0m"),
                _ => eprintln!("\x1b[31m✗ Comando `say` non disponibile (solo macOS)\x1b[0m"),
            }
        }
    }
}

// ── Settings management ──

fn find_config_path() -> Option<&'static str> {
    CONFIG_PATHS.iter().find(|p| std::path::Path::new(p).exists()).copied()
}

fn load_config() -> Option<serde_json::Value> {
    let path = find_config_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

fn save_config(config: &serde_json::Value) -> Result<(), String> {
    let path = find_config_path().ok_or("Config non trovato")?;
    let yaml = serde_yaml::to_string(config).map_err(|e| e.to_string())?;
    std::fs::write(path, yaml).map_err(|e| e.to_string())
}

fn show_settings() {
    if let Some(config) = load_config() {
        let backend = config.pointer("/llm/default_backend").and_then(|v| v.as_str()).unwrap_or("?");
        let ollama_model = config.pointer("/ollama/model").and_then(|v| v.as_str()).unwrap_or("?");
        let ollama_url = config.pointer("/ollama/url").and_then(|v| v.as_str()).unwrap_or("?");
        let openai_model = config.pointer("/openai/model").and_then(|v| v.as_str()).unwrap_or("?");
        let openai_key = config.pointer("/openai/api_key").and_then(|v| v.as_str()).unwrap_or("");
        let claude_model = config.pointer("/claude/model").and_then(|v| v.as_str()).unwrap_or("?");
        let claude_key = config.pointer("/claude/api_key").and_then(|v| v.as_str()).unwrap_or("");

        let mask = |k: &str| if k.len() > 8 { format!("{}...{}", &k[..4], &k[k.len()-4..]) } else if k.is_empty() { "(vuota)".into() } else { "***".into() };

        println!("\x1b[36m── Impostazioni ──\x1b[0m");
        println!("  \x1b[1mProvider attivo:\x1b[0m  \x1b[32m{}\x1b[0m", backend);
        println!();
        println!("  \x1b[1mOllama:\x1b[0m");
        println!("    URL:     {}", ollama_url);
        println!("    Modello: {}", ollama_model);
        println!();
        println!("  \x1b[1mOpenAI:\x1b[0m");
        println!("    Modello: {}", openai_model);
        println!("    API Key: {}", mask(openai_key));
        println!();
        println!("  \x1b[1mClaude:\x1b[0m");
        println!("    Modello: {}", claude_model);
        println!("    API Key: {}", mask(claude_key));
        println!("\x1b[36m──────────────────\x1b[0m");
        println!("  \x1b[2mUsa /provider, /model, /key per modificare. /restart per applicare.\x1b[0m");
    } else {
        eprintln!("\x1b[31m✗ Config non trovato\x1b[0m");
    }
}

fn set_provider(provider: &str) {
    if !["ollama", "openai", "claude"].contains(&provider) {
        println!("\x1b[31m✗ Provider non valido. Usa: ollama, openai, claude\x1b[0m");
        return;
    }
    if let Some(mut config) = load_config() {
        config["llm"]["default_backend"] = serde_json::Value::String(provider.to_string());
        config["llm"]["complex_backend"] = serde_json::Value::String(provider.to_string());
        match save_config(&config) {
            Ok(_) => println!("\x1b[32m✓ Provider impostato: {}\x1b[0m\n  \x1b[2mUsa /restart per applicare\x1b[0m", provider),
            Err(e) => eprintln!("\x1b[31m✗ Errore salvataggio: {}\x1b[0m", e),
        }
    }
}

fn set_model(model: &str) {
    if let Some(mut config) = load_config() {
        let backend = config.pointer("/llm/default_backend").and_then(|v| v.as_str()).unwrap_or("openai").to_string();
        match backend.as_str() {
            "ollama" => config["ollama"]["model"] = serde_json::Value::String(model.to_string()),
            "openai" => config["openai"]["model"] = serde_json::Value::String(model.to_string()),
            "claude" => config["claude"]["model"] = serde_json::Value::String(model.to_string()),
            _ => {}
        }
        match save_config(&config) {
            Ok(_) => println!("\x1b[32m✓ Modello impostato: {} (provider: {})\x1b[0m\n  \x1b[2mUsa /restart per applicare\x1b[0m", model, backend),
            Err(e) => eprintln!("\x1b[31m✗ Errore salvataggio: {}\x1b[0m", e),
        }
    }
}

fn set_api_key(provider: &str, key: &str) {
    if !["openai", "claude"].contains(&provider) {
        println!("\x1b[31m✗ Provider non valido per API key. Usa: openai, claude\x1b[0m");
        return;
    }
    if let Some(mut config) = load_config() {
        config[provider]["api_key"] = serde_json::Value::String(key.to_string());
        match save_config(&config) {
            Ok(_) => println!("\x1b[32m✓ API key {} salvata\x1b[0m\n  \x1b[2mUsa /restart per applicare\x1b[0m", provider),
            Err(e) => eprintln!("\x1b[31m✗ Errore salvataggio: {}\x1b[0m", e),
        }
    }
}

// ── Ollama commands ──

fn handle_ollama_command(arg: &str) {
    let parts: Vec<&str> = arg.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| *s).unwrap_or("");
    let subarg = parts.get(1).map(|s| *s).unwrap_or("");

    match subcmd {
        "login" => {
            println!("\x1b[36m⟳ Avvio login Ollama...\x1b[0m");
            let status = std::process::Command::new("ollama")
                .arg("login")
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status();
            match status {
                Ok(s) if s.success() => println!("\x1b[32m✓ Login Ollama completato\x1b[0m"),
                Ok(_) => println!("\x1b[31m✗ Login fallito\x1b[0m"),
                Err(_) => println!("\x1b[31m✗ Ollama non installato. Installalo da https://ollama.com\x1b[0m"),
            }
        }
        "models" | "list" => {
            println!("\x1b[36m⟳ Modelli Ollama disponibili:\x1b[0m");
            let output = std::process::Command::new("ollama").arg("list").output();
            match output {
                Ok(o) => println!("{}", String::from_utf8_lossy(&o.stdout)),
                Err(_) => println!("\x1b[31m✗ Ollama non raggiungibile\x1b[0m"),
            }
        }
        "pull" => {
            if subarg.is_empty() {
                println!("\x1b[33mUso: /ollama pull <modello>\x1b[0m");
                println!("  Esempi: llama3.2, kimi-k2.5:cloud, mistral, gemma2");
            } else {
                println!("\x1b[36m⟳ Download modello {}...\x1b[0m", subarg);
                let status = std::process::Command::new("ollama")
                    .args(["pull", subarg])
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status();
                match status {
                    Ok(s) if s.success() => {
                        println!("\x1b[32m✓ Modello {} scaricato\x1b[0m", subarg);
                        println!("  \x1b[2mPer usarlo: /provider ollama → /model {} → /restart\x1b[0m", subarg);
                    }
                    Ok(_) => println!("\x1b[31m✗ Download fallito\x1b[0m"),
                    Err(_) => println!("\x1b[31m✗ Ollama non installato\x1b[0m"),
                }
            }
        }
        "url" => {
            if subarg.is_empty() {
                if let Some(config) = load_config() {
                    let url = config.pointer("/ollama/url").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("  URL Ollama: {}", url);
                }
            } else {
                if let Some(mut config) = load_config() {
                    config["ollama"]["url"] = serde_json::Value::String(subarg.to_string());
                    match save_config(&config) {
                        Ok(_) => println!("\x1b[32m✓ URL Ollama: {}\x1b[0m\n  \x1b[2m/restart per applicare\x1b[0m", subarg),
                        Err(e) => eprintln!("\x1b[31m✗ {}\x1b[0m", e),
                    }
                }
            }
        }
        "" => {
            println!("\x1b[33mUso: /ollama <login|models|pull|url>\x1b[0m");
        }
        _ => {
            println!("\x1b[33mSottocomando sconosciuto: {}. Usa: login, models, pull, url\x1b[0m", subcmd);
        }
    }
}

// ── Comandi /fs — gestione agent-fs ──

fn handle_fs_command(arg: &str, socket_path: &str, req_id: &mut u64) {
    let parts: Vec<&str> = arg.splitn(2, ' ').collect();
    let subcmd = parts.first().copied().unwrap_or("");
    let subarg = parts.get(1).copied().unwrap_or("");

    match subcmd {
        "start" => {
            println!("\x1b[36m⟳ Avvio agent-fs...\x1b[0m");

            // Percorso socket agent-fs
            let fs_socket = if std::path::Path::new("/tmp/agentd-fs.sock").exists() {
                "/tmp/agentd-fs.sock"
            } else if std::path::Path::new("/run/agentd-fs.sock").exists() {
                "/run/agentd-fs.sock"
            } else {
                ""
            };

            // Controlla se è già attivo
            if !fs_socket.is_empty() {
                if std::os::unix::net::UnixStream::connect(fs_socket).is_ok() {
                    println!("\x1b[32m✓ agent-fs è già in esecuzione\x1b[0m");
                    return;
                }
            }

            // Avvia agent-fs come processo figlio
            let child = std::process::Command::new("sh")
                .arg("-c")
                .arg("cargo run -p agent-fs &>/tmp/agent-fs.log &")
                .spawn();

            match child {
                Ok(_) => {
                    // Attendi che il socket appaia
                    for _ in 0..10 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if std::path::Path::new("/tmp/agentd-fs.sock").exists() {
                            println!("\x1b[32m✓ agent-fs avviato. Indicizzazione in corso.\x1b[0m");
                            return;
                        }
                    }
                    println!("\x1b[33m⟳ agent-fs avviato, ma il socket non è ancora pronto. Attendi qualche secondo.\x1b[0m");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗ Errore avvio agent-fs: {}\x1b[0m", e);
                }
            }
        }

        "status" => {
            // Percorso socket agent-fs
            let fs_socket = if std::path::Path::new("/tmp/agentd-fs.sock").exists() {
                Some("/tmp/agentd-fs.sock")
            } else if std::path::Path::new("/run/agentd-fs.sock").exists() {
                Some("/run/agentd-fs.sock")
            } else {
                None
            };

            let fs_socket = match fs_socket {
                Some(s) => s,
                None => {
                    println!("\x1b[33m● agent-fs non in esecuzione\x1b[0m");
                    println!("  \x1b[2mUsa /fs start per avviarlo\x1b[0m");
                    return;
                }
            };

            // Prova a connettersi e chiedere lo stato
            match std::os::unix::net::UnixStream::connect(fs_socket) {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
                    let status_req = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "fs.status",
                        "params": { "type": "fs.status" },
                        "id": 1
                    });
                    let req_json = serde_json::to_string(&status_req).unwrap_or_default();
                    writeln!(stream, "{}", req_json).ok();

                    let mut reader = io::BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                        if let Ok(resp) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                            if let Some(result) = resp.get("result") {
                                let total = result.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                                let indexed = result.get("indexed_files").and_then(|v| v.as_u64()).unwrap_or(0);
                                let pending = result.get("pending_files").and_then(|v| v.as_u64()).unwrap_or(0);

                                println!("\x1b[32m● agent-fs attivo\x1b[0m");
                                println!("  File totali:     {}", total);
                                println!("  Indicizzati:     {}", indexed);
                                println!("  In attesa:       {}", pending);
                                return;
                            }
                        }
                    }
                    println!("\x1b[32m● agent-fs attivo\x1b[0m (stato dettagliato non disponibile)");
                }
                Err(_) => {
                    println!("\x1b[31m● agent-fs non raggiungibile\x1b[0m (socket orfano?)");
                    println!("  \x1b[2mUsa /fs start per riavviarlo\x1b[0m");
                }
            }
        }

        "search" => {
            if subarg.is_empty() {
                println!("\x1b[33mUso: /fs search <query>\x1b[0m");
                println!("  Esempio: /fs search documenti sulla privacy");
                return;
            }

            // Invia la ricerca a agentd come SearchRequest
            let msg = ShellToAgent::SearchRequest { query: subarg.to_string() };
            let req = match JsonRpcRequest::from_shell_message(&msg, *req_id) {
                Ok(r) => r,
                Err(e) => { eprintln!("\x1b[31mErrore: {}\x1b[0m", e); return; }
            };
            *req_id += 1;

            let json = serde_json::to_string(&req).unwrap_or_default();
            match std::os::unix::net::UnixStream::connect(socket_path) {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
                    writeln!(stream, "{}", json).ok();
                    let mut reader = io::BufReader::new(&stream);
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(_) if !line.is_empty() => {
                            if let Ok(resp) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                                if let Some(text) = resp.pointer("/result/text").and_then(|t| t.as_str()) {
                                    if text.is_empty() {
                                        println!("\x1b[33mNessun risultato per '{}'\x1b[0m", subarg);
                                    } else {
                                        println!("\x1b[36m── Risultati ricerca: {} ──\x1b[0m", subarg);
                                        println!("{}", text);
                                        println!("\x1b[36m──────────────────────────\x1b[0m");
                                    }
                                } else {
                                    println!("\x1b[33mRisposta inattesa da agentd\x1b[0m");
                                }
                            }
                        }
                        _ => eprintln!("\x1b[31m✗ Nessuna risposta (timeout)\x1b[0m"),
                    }
                }
                Err(e) => eprintln!("\x1b[31m✗ Connessione ad agentd fallita: {}\x1b[0m", e),
            }
        }

        "" => {
            println!("\x1b[33mUso: /fs <start|status|search>\x1b[0m");
            println!("  /fs start           — avvia agent-fs");
            println!("  /fs status          — stato del servizio");
            println!("  /fs search <query>  — ricerca semantica nei file");
        }

        _ => {
            println!("\x1b[33mSottocomando /fs sconosciuto: {}. Usa: start, status, search\x1b[0m", subcmd);
        }
    }
}

// ── Comunicazione con agentd ──

fn send_briefing(socket_path: &str, req_id: &mut u64) {
    let briefing_msg = ShellToAgent::BriefingRequest;
    if let Some(req) = JsonRpcRequest::from_shell_message(&briefing_msg, *req_id).ok() {
        *req_id += 1;
        let json = serde_json::to_string(&req).unwrap_or_default();
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket_path) {
            stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
            writeln!(stream, "{}", json).ok();
            let mut reader = io::BufReader::new(&stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                if let Ok(resp) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    if let Some(text) = resp.pointer("/result/text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            println!("\x1b[33m── Briefing ──\x1b[0m");
                            println!("{}", text);
                            println!("\x1b[33m──────────────\x1b[0m\n");
                        }
                    }
                }
            }
        }
    }
}

// ============================================================
// Feature 3: Invio con gestione conferma zona gialla
// ============================================================

/// Invia un messaggio all'agente e gestisce la risposta, incluse le richieste
/// di conferma per azioni in zona gialla (interactive yellow zone).
fn send_to_agent_with_confirm(text: &str, socket_path: &str, req_id: &mut u64) {
    let msg = ShellToAgent::UserInput { text: text.to_string() };
    let req = match JsonRpcRequest::from_shell_message(&msg, *req_id) {
        Ok(r) => r,
        Err(e) => { eprintln!("\x1b[31mErrore: {}\x1b[0m", e); return; }
    };
    *req_id += 1;

    let json = serde_json::to_string(&req).unwrap_or_default();
    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(mut stream) => {
            stream.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
            writeln!(stream, "{}", json).ok();
            let mut reader = io::BufReader::new(&stream);
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(_) if !line.is_empty() => {
                    // Controlla se la risposta è una richiesta di conferma (Feature 3)
                    if let Ok(resp) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                        if let Some(result) = resp.get("result") {
                            let msg_type = result.get("type").and_then(|t| t.as_str());
                            let needs_confirm = result.get("needs_confirm").and_then(|v| v.as_bool()).unwrap_or(false);

                            if msg_type == Some("agent.confirm_request") || needs_confirm {
                                // Risposta con richiesta di conferma
                                handle_confirm_request(result, socket_path, req_id);
                                return;
                            }
                        }
                    }
                    // Risposta normale
                    display_response(line.trim());
                }
                _ => eprintln!("\x1b[31m✗ Nessuna risposta (timeout)\x1b[0m"),
            }
        }
        Err(e) => eprintln!("\x1b[31m✗ Connessione fallita: {}\x1b[0m", e),
    }
}

/// Gestisce una richiesta di conferma per azione in zona gialla.
/// Mostra la descrizione dell'azione e chiede [y/n] all'utente.
fn handle_confirm_request(result: &serde_json::Value, socket_path: &str, req_id: &mut u64) {
    let action_id = result.get("action_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let description = result.get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("Azione sconosciuta");

    // Mostra la richiesta di conferma
    println!("\x1b[33m⚠ Zona gialla — azione che richiede conferma:\x1b[0m");
    println!("  \x1b[1m{}\x1b[0m", description);
    println!();
    print!("\x1b[33mEseguire? [y/n]:\x1b[0m ");
    io::stdout().flush().unwrap();

    // Leggi la risposta dell'utente
    let stdin = io::stdin();
    let mut input = String::new();
    if stdin.lock().read_line(&mut input).is_err() {
        println!("\x1b[31m✗ Errore lettura input\x1b[0m");
        return;
    }

    let answer = input.trim().to_lowercase();
    let approved = answer == "y" || answer == "yes" || answer == "si" || answer == "s";

    // Invia la conferma/rifiuto ad agentd
    let confirm_msg = ShellToAgent::UserConfirm {
        action_id: action_id.to_string(),
        approved,
    };
    let confirm_req = match JsonRpcRequest::from_shell_message(&confirm_msg, *req_id) {
        Ok(r) => r,
        Err(e) => { eprintln!("\x1b[31mErrore: {}\x1b[0m", e); return; }
    };
    *req_id += 1;

    let json = serde_json::to_string(&confirm_req).unwrap_or_default();
    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(mut stream) => {
            stream.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
            writeln!(stream, "{}", json).ok();
            let mut reader = io::BufReader::new(&stream);
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(_) if !line.is_empty() => display_response(line.trim()),
                _ => eprintln!("\x1b[31m✗ Nessuna risposta (timeout)\x1b[0m"),
            }
        }
        Err(e) => eprintln!("\x1b[31m✗ Connessione fallita: {}\x1b[0m", e),
    }
}

fn display_response(raw: &str) {
    if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(raw) {
        if let Some(result) = resp.result {
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let zone = result.get("zone").and_then(|z| z.as_str());
            let commands = result.get("commands").and_then(|c| c.as_array());

            let badge = match zone {
                Some("green") => "\x1b[32m●\x1b[0m",
                Some("yellow") => "\x1b[33m⚠\x1b[0m",
                Some("red") => "\x1b[31m⛔\x1b[0m",
                _ => "\x1b[34m●\x1b[0m",
            };

            let parts: Vec<&str> = text.splitn(2, "\n\n").collect();
            if parts.len() >= 2 && commands.map(|c| !c.is_empty()).unwrap_or(false) {
                println!("{} \x1b[1m{}\x1b[0m", badge, parts[0]);
                println!("\x1b[2m{}\x1b[0m", "─".repeat(60));
                println!("\x1b[32m{}\x1b[0m", parts[1]);
            } else {
                println!("{} {}", badge, text);
            }

            if let Some(cmds) = commands {
                if !cmds.is_empty() {
                    let cmd_list: Vec<String> = cmds.iter().filter_map(|c| c.as_str().map(String::from)).collect();
                    println!("\x1b[2m  → {}\x1b[0m", cmd_list.join(", "));
                }
            }
        } else if let Some(error) = resp.error {
            eprintln!("\x1b[31m✗ {}\x1b[0m", error.message);
        }
    } else {
        println!("{}", raw);
    }
}
