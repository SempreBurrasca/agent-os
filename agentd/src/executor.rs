//! Executor — esecuzione comandi con sandbox opzionale (bubblewrap).
//!
//! Comandi nella whitelist: esecuzione diretta.
//! Tutto il resto: esecuzione in sandbox bwrap (se configurato).

use anyhow::{Result, anyhow};
use agentos_common::config::{SecurityConfig, SandboxConfig};
use agentos_common::types::ExecutionResult;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

/// L'Executor esegue comandi in modo sicuro.
pub struct Executor {
    /// Comandi nella whitelist — esecuzione diretta
    whitelist: Vec<String>,
    /// Se true, i comandi non in whitelist vanno in sandbox
    sandbox_by_default: bool,
    /// Timeout per comandi in sandbox (secondi)
    sandbox_timeout: Duration,
    /// Cartelle scrivibili in sandbox
    writable_paths: Vec<String>,
    /// Se true, la rete è disabilitata in sandbox
    disable_network: bool,
}

impl Executor {
    /// Crea un nuovo Executor dalla configurazione.
    pub fn new(security: &SecurityConfig, sandbox: &SandboxConfig) -> Self {
        Self {
            whitelist: security.command_whitelist.clone(),
            sandbox_by_default: security.sandbox_by_default,
            sandbox_timeout: Duration::from_secs(sandbox.timeout_secs),
            writable_paths: sandbox.writable_paths.clone(),
            disable_network: sandbox.disable_network,
        }
    }

    /// Esegue un comando, decidendo automaticamente se usare la sandbox.
    pub async fn execute(&self, command: &str, working_dir: Option<&str>) -> Result<ExecutionResult> {
        let should_sandbox = self.should_sandbox(command);

        if should_sandbox {
            self.execute_sandboxed(command, working_dir).await
        } else {
            self.execute_direct(command, working_dir).await
        }
    }

    /// Determina se un comando deve girare in sandbox.
    fn should_sandbox(&self, command: &str) -> bool {
        if !self.sandbox_by_default {
            return false;
        }

        let first_token = command.trim().split_whitespace().next().unwrap_or("");
        !self.whitelist.contains(&first_token.to_string())
    }

    /// Esecuzione diretta (senza sandbox).
    async fn execute_direct(&self, command: &str, working_dir: Option<&str>) -> Result<ExecutionResult> {
        debug!(command = command, "Esecuzione diretta");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let output = timeout(self.sandbox_timeout, cmd.output())
            .await
            .map_err(|_| anyhow!("Timeout esecuzione comando"))??;

        Ok(ExecutionResult {
            command: command.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            return_code: output.status.code().unwrap_or(-1),
            sandboxed: false,
            timed_out: false,
        })
    }

    /// Esecuzione in sandbox con bubblewrap (bwrap).
    async fn execute_sandboxed(&self, command: &str, working_dir: Option<&str>) -> Result<ExecutionResult> {
        info!(command = command, "Esecuzione in sandbox (bwrap)");

        let work_dir = working_dir.unwrap_or("/tmp");

        // Costruisci il comando bwrap
        let mut bwrap_args = vec![
            "--ro-bind".to_string(), "/".to_string(), "/".to_string(),
            "--dev".to_string(), "/dev".to_string(),
            "--tmpfs".to_string(), "/tmp".to_string(),
            "--bind".to_string(), work_dir.to_string(), work_dir.to_string(),
            "--die-with-parent".to_string(),
        ];

        // Aggiungi cartelle scrivibili dalla config
        for path in &self.writable_paths {
            bwrap_args.push("--bind".to_string());
            bwrap_args.push(path.clone());
            bwrap_args.push(path.clone());
        }

        // Disabilita rete se configurato
        if self.disable_network {
            bwrap_args.push("--unshare-net".to_string());
        }

        // Aggiungi il comando da eseguire
        bwrap_args.push("sh".to_string());
        bwrap_args.push("-c".to_string());
        bwrap_args.push(command.to_string());

        let mut cmd = Command::new("bwrap");
        cmd.args(&bwrap_args);

        let result = timeout(self.sandbox_timeout, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                Ok(ExecutionResult {
                    command: command.to_string(),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    return_code: output.status.code().unwrap_or(-1),
                    sandboxed: true,
                    timed_out: false,
                })
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Errore esecuzione bwrap — fallback a esecuzione diretta");
                // Fallback: se bwrap non è disponibile, esegui direttamente
                // (ad esempio in ambiente di sviluppo senza bwrap)
                self.execute_direct(command, working_dir).await
            }
            Err(_) => {
                warn!(command = command, "Timeout esecuzione sandbox");
                Ok(ExecutionResult {
                    command: command.to_string(),
                    stdout: String::new(),
                    stderr: "Comando interrotto per timeout".to_string(),
                    return_code: -1,
                    sandboxed: true,
                    timed_out: true,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_executor() -> Executor {
        let security = SecurityConfig {
            yellow_patterns: vec![],
            command_whitelist: vec![
                "ls".to_string(), "cat".to_string(), "pwd".to_string(),
                "echo".to_string(), "date".to_string(),
            ],
            sandbox_by_default: true,
        };
        let sandbox = SandboxConfig {
            timeout_secs: 10,
            writable_paths: vec!["/tmp".to_string()],
            disable_network: true,
        };
        Executor::new(&security, &sandbox)
    }

    #[test]
    fn test_should_sandbox_whitelist() {
        let executor = test_executor();
        assert!(!executor.should_sandbox("ls -la"));
        assert!(!executor.should_sandbox("echo hello"));
        assert!(!executor.should_sandbox("cat file.txt"));
    }

    #[test]
    fn test_should_sandbox_non_whitelist() {
        let executor = test_executor();
        assert!(executor.should_sandbox("python3 script.py"));
        assert!(executor.should_sandbox("gcc main.c"));
        assert!(executor.should_sandbox("node app.js"));
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let executor = test_executor();
        let result = executor.execute("echo hello", None).await.unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.return_code, 0);
        assert!(!result.sandboxed);  // echo è in whitelist
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_execute_pwd() {
        let executor = test_executor();
        let result = executor.execute("pwd", Some("/tmp")).await.unwrap();
        assert!(result.stdout.contains("/tmp") || result.stdout.contains("/private/tmp"));
        assert_eq!(result.return_code, 0);
    }

    #[tokio::test]
    async fn test_execute_failing_command() {
        let executor = test_executor();
        let result = executor.execute("ls /nonexistent_dir_12345", None).await.unwrap();
        assert_ne!(result.return_code, 0);
        assert!(!result.stderr.is_empty());
    }

    #[tokio::test]
    async fn test_execute_with_working_dir() {
        let executor = test_executor();
        let result = executor.execute("ls", Some("/tmp")).await.unwrap();
        assert_eq!(result.return_code, 0);
    }
}
