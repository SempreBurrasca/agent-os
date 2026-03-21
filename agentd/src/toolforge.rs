//! ToolForge — sistema di auto-sviluppo tool per l'agente.
//!
//! L'agente può creare nuovi tool (script Python/Bash/Node), salvarli,
//! registrarli dinamicamente, e usarli nelle sessioni future.
//! Può anche creare app complete (web app, script, utility).
//!
//! I tool creati vengono salvati in ~/.agentos/tools/ con un manifest.json
//! che descrive nome, descrizione, linguaggio, e parametri.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, debug, warn};

/// Directory dove vengono salvati i tool custom
fn tools_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".agentos").join("tools")
}

/// Directory dove vengono salvate le app create
fn apps_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".agentos").join("apps")
}

/// Manifest di un tool custom creato dall'agente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomTool {
    /// Nome univoco del tool (snake_case)
    pub name: String,
    /// Descrizione per l'LLM
    pub description: String,
    /// Linguaggio dello script (python, bash, node)
    pub language: String,
    /// Nome del file script (relativo a tools_dir)
    pub script_file: String,
    /// Parametri accettati dal tool
    pub parameters: Vec<ToolParam>,
    /// Data di creazione
    pub created_at: String,
    /// Quante volte è stato usato
    pub use_count: u64,
}

/// Parametro di un tool custom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// Manifest di un'app creata dall'agente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomApp {
    pub name: String,
    pub description: String,
    pub language: String,
    pub entry_file: String,
    pub created_at: String,
}

/// Registro dei tool custom — caricato da disco all'avvio.
pub struct ToolForge {
    tools: HashMap<String, CustomTool>,
    apps: HashMap<String, CustomApp>,
}

impl ToolForge {
    /// Carica il registro dai file su disco.
    pub fn load() -> Self {
        let mut forge = Self {
            tools: HashMap::new(),
            apps: HashMap::new(),
        };

        // Carica tool custom
        let manifest_path = tools_dir().join("manifest.json");
        if manifest_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(tools) = serde_json::from_str::<Vec<CustomTool>>(&content) {
                    for tool in tools {
                        forge.tools.insert(tool.name.clone(), tool);
                    }
                }
            }
        }

        // Carica app
        let apps_manifest = apps_dir().join("manifest.json");
        if apps_manifest.exists() {
            if let Ok(content) = std::fs::read_to_string(&apps_manifest) {
                if let Ok(apps) = serde_json::from_str::<Vec<CustomApp>>(&content) {
                    for app in apps {
                        forge.apps.insert(app.name.clone(), app);
                    }
                }
            }
        }

        info!(tools = forge.tools.len(), apps = forge.apps.len(), "ToolForge caricato");
        forge
    }

    /// Salva il manifest su disco.
    fn save_tools_manifest(&self) {
        let dir = tools_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tools: Vec<&CustomTool> = self.tools.values().collect();
        if let Ok(json) = serde_json::to_string_pretty(&tools) {
            let _ = std::fs::write(dir.join("manifest.json"), json);
        }
    }

    fn save_apps_manifest(&self) {
        let dir = apps_dir();
        let _ = std::fs::create_dir_all(&dir);
        let apps: Vec<&CustomApp> = self.apps.values().collect();
        if let Ok(json) = serde_json::to_string_pretty(&apps) {
            let _ = std::fs::write(dir.join("manifest.json"), json);
        }
    }

    /// Crea un nuovo tool custom. Salva lo script e aggiorna il manifest.
    pub fn create_tool(
        &mut self,
        name: &str,
        description: &str,
        language: &str,
        code: &str,
        parameters: Vec<ToolParam>,
    ) -> Result<String, String> {
        // Validazione nome
        let name = name.to_lowercase().replace(' ', "_").replace('-', "_");
        if name.is_empty() || name.len() > 50 {
            return Err("Nome tool non valido (1-50 caratteri, snake_case)".into());
        }

        // Determina estensione
        let ext = match language {
            "python" | "py" => "py",
            "bash" | "sh" => "sh",
            "node" | "javascript" | "js" => "js",
            _ => return Err(format!("Linguaggio '{}' non supportato. Usa: python, bash, node", language)),
        };

        let script_file = format!("{}.{}", name, ext);
        let script_path = tools_dir().join(&script_file);
        let _ = std::fs::create_dir_all(tools_dir());

        // Scrivi lo script
        let shebang = match ext {
            "py" => "#!/usr/bin/env python3\n",
            "sh" => "#!/bin/bash\n",
            "js" => "#!/usr/bin/env node\n",
            _ => "",
        };
        let full_code = format!("{}{}", shebang, code);
        std::fs::write(&script_path, &full_code).map_err(|e| format!("Errore scrittura: {}", e))?;

        // Rendi eseguibile
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
        }

        // Registra nel manifest
        let tool = CustomTool {
            name: name.clone(),
            description: description.to_string(),
            language: language.to_string(),
            script_file,
            parameters,
            created_at: chrono::Utc::now().to_rfc3339(),
            use_count: 0,
        };
        self.tools.insert(name.clone(), tool);
        self.save_tools_manifest();

        info!(name = %name, language = language, "Nuovo tool creato");
        Ok(format!("Tool '{}' creato in {}", name, script_path.display()))
    }

    /// Esegue un tool custom con gli argomenti forniti.
    pub async fn execute_tool(&mut self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let tool = self.tools.get(name).ok_or(format!("Tool '{}' non trovato", name))?.clone();

        let script_path = tools_dir().join(&tool.script_file);
        if !script_path.exists() {
            return Err(format!("Script '{}' non trovato su disco", tool.script_file));
        }

        // Costruisci il comando
        let interpreter = match tool.language.as_str() {
            "python" | "py" => "python3",
            "bash" | "sh" => "bash",
            "node" | "javascript" | "js" => "node",
            _ => return Err("Linguaggio non supportato".into()),
        };

        // Passa gli argomenti come JSON via stdin o come variabili d'ambiente
        let args_json = serde_json::to_string(args).unwrap_or_default();

        let output = tokio::process::Command::new(interpreter)
            .arg(script_path.to_str().unwrap_or(""))
            .env("TOOL_ARGS", &args_json)
            .env("TOOL_NAME", name)
            .output()
            .await
            .map_err(|e| format!("Errore esecuzione: {}", e))?;

        // Incrementa contatore uso
        if let Some(t) = self.tools.get_mut(name) {
            t.use_count += 1;
            self.save_tools_manifest();
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Err(format!("Exit code {}: {}", output.status.code().unwrap_or(-1), if stderr.is_empty() { &stdout } else { &stderr }))
        }
    }

    /// Crea un'app (progetto più complesso di un singolo script).
    pub fn create_app(
        &mut self,
        name: &str,
        description: &str,
        language: &str,
        files: &HashMap<String, String>, // nome_file -> contenuto
    ) -> Result<String, String> {
        let name = name.to_lowercase().replace(' ', "_").replace('-', "_");
        let app_path = apps_dir().join(&name);
        std::fs::create_dir_all(&app_path).map_err(|e| format!("Errore creazione dir: {}", e))?;

        let mut entry_file = String::new();
        for (filename, content) in files {
            let file_path = app_path.join(filename);
            // Crea sottodirectory se necessario
            if let Some(parent) = file_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&file_path, content).map_err(|e| format!("Errore scrittura {}: {}", filename, e))?;

            // Rendi eseguibile se è uno script
            #[cfg(unix)]
            if filename.ends_with(".sh") || filename.ends_with(".py") || filename.ends_with(".js") {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o755));
            }

            if entry_file.is_empty() {
                entry_file = filename.clone();
            }
        }

        let app = CustomApp {
            name: name.clone(),
            description: description.to_string(),
            language: language.to_string(),
            entry_file,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.apps.insert(name.clone(), app);
        self.save_apps_manifest();

        info!(name = %name, files = files.len(), "App creata");
        Ok(format!("App '{}' creata in {}", name, app_path.display()))
    }

    /// Verifica se un tool custom esiste.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Restituisce linguaggio e path dello script (per esecuzione fuori dal lock).
    pub fn get_tool_info(&self, name: &str) -> Option<(String, String)> {
        self.tools.get(name).map(|t| {
            (t.language.clone(), tools_dir().join(&t.script_file).to_string_lossy().to_string())
        })
    }

    /// Incrementa il contatore di uso di un tool.
    pub fn increment_use(&mut self, name: &str) {
        if let Some(t) = self.tools.get_mut(name) {
            t.use_count += 1;
            self.save_tools_manifest();
        }
    }

    /// Restituisce la descrizione dei tool custom per il prompt LLM.
    pub fn tools_description(&self) -> String {
        if self.tools.is_empty() {
            return String::new();
        }

        let mut desc = String::from("\n\nTOOL CUSTOM (creati dall'agente):\n");
        for tool in self.tools.values() {
            let params: Vec<String> = tool.parameters.iter()
                .map(|p| format!("\"{}\"", p.name))
                .collect();
            desc.push_str(&format!(
                "- custom_{}: {{{}}}  — {} ({})\n",
                tool.name,
                params.join(", "),
                tool.description,
                tool.language
            ));
        }
        desc.push_str("\nPer usare un tool custom, usa: {\"tool\": \"custom_<nome>\", \"args\": {...}}\n");
        desc
    }

    /// Lista tutti i tool custom.
    pub fn list_tools(&self) -> Vec<&CustomTool> {
        self.tools.values().collect()
    }

    /// Lista tutte le app.
    pub fn list_apps(&self) -> Vec<&CustomApp> {
        self.apps.values().collect()
    }

    /// Elimina un tool.
    pub fn delete_tool(&mut self, name: &str) -> Result<(), String> {
        if let Some(tool) = self.tools.remove(name) {
            let path = tools_dir().join(&tool.script_file);
            let _ = std::fs::remove_file(path);
            self.save_tools_manifest();
            Ok(())
        } else {
            Err(format!("Tool '{}' non trovato", name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_dir() {
        let dir = tools_dir();
        assert!(dir.to_str().unwrap().contains(".agentos"));
    }

    #[test]
    fn test_toolforge_load_empty() {
        let forge = ToolForge::load();
        // Non crasha anche se non ci sono file
        assert!(forge.tools_description().is_empty() || forge.tools_description().contains("TOOL CUSTOM"));
    }

    #[test]
    fn test_custom_tool_serde() {
        let tool = CustomTool {
            name: "test_tool".into(),
            description: "Un tool di test".into(),
            language: "python".into(),
            script_file: "test_tool.py".into(),
            parameters: vec![ToolParam { name: "input".into(), description: "testo".into(), required: true }],
            created_at: "2024-01-01".into(),
            use_count: 0,
        };
        let json = serde_json::to_string(&tool).unwrap();
        let parsed: CustomTool = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test_tool");
    }
}
