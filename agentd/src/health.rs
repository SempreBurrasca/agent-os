//! Health Monitor — monitoraggio salute del sistema.
//!
//! Controlla periodicamente: connettività di rete, stato servizi,
//! spazio disco, carico CPU. Invia notifiche se qualcosa cambia.
//! Supporta il self-healing: se la rete cade, notifica l'utente.

use tracing::{info, debug};
use tokio::process::Command;

/// Stato della connettività di rete.
#[derive(Debug, Clone, PartialEq)]
pub enum NetworkState {
    Connected,
    Disconnected,
    Unknown,
}

/// Stato del sistema.
#[derive(Debug, Clone)]
pub struct SystemHealth {
    /// Connettività di rete
    pub network: NetworkState,
    /// Spazio disco disponibile sulla root (percentuale usata)
    pub disk_usage_pct: Option<u8>,
    /// Carico medio del sistema (1 min)
    pub load_avg: Option<f64>,
    /// Memoria disponibile (MB)
    pub mem_available_mb: Option<u64>,
    /// Se Ollama è raggiungibile
    pub ollama_available: bool,
}

/// Monitor di salute del sistema.
pub struct HealthMonitor {
    /// Stato precedente (per rilevare cambiamenti)
    last_state: Option<SystemHealth>,
    /// URL di Ollama
    ollama_url: String,
}

impl HealthMonitor {
    /// Crea un nuovo monitor.
    pub fn new(ollama_url: &str) -> Self {
        Self {
            last_state: None,
            ollama_url: ollama_url.to_string(),
        }
    }

    /// Esegue un check completo. Restituisce lo stato e gli eventuali cambiamenti.
    pub async fn check(&mut self) -> (SystemHealth, Vec<HealthChange>) {
        let health = SystemHealth {
            network: Self::check_network().await,
            disk_usage_pct: Self::check_disk().await,
            load_avg: Self::check_load().await,
            mem_available_mb: Self::check_memory().await,
            ollama_available: self.check_ollama().await,
        };

        let changes = self.detect_changes(&health);
        self.last_state = Some(health.clone());

        (health, changes)
    }

    /// Rileva i cambiamenti rispetto allo stato precedente.
    fn detect_changes(&self, current: &SystemHealth) -> Vec<HealthChange> {
        let mut changes = Vec::new();

        if let Some(ref prev) = self.last_state {
            // Rete: cambiamento di stato
            if prev.network != current.network {
                match current.network {
                    NetworkState::Disconnected => {
                        changes.push(HealthChange::NetworkDown);
                    }
                    NetworkState::Connected if prev.network == NetworkState::Disconnected => {
                        changes.push(HealthChange::NetworkRestored);
                    }
                    _ => {}
                }
            }

            // Disco: avviso se supera l'85%
            if let Some(pct) = current.disk_usage_pct {
                let prev_pct = prev.disk_usage_pct.unwrap_or(0);
                if pct >= 85 && prev_pct < 85 {
                    changes.push(HealthChange::DiskSpaceLow(pct));
                }
            }

            // Memoria: avviso se scende sotto 500MB
            if let Some(mb) = current.mem_available_mb {
                let prev_mb = prev.mem_available_mb.unwrap_or(u64::MAX);
                if mb < 500 && prev_mb >= 500 {
                    changes.push(HealthChange::MemoryLow(mb));
                }
            }

            // Ollama: cambiamento disponibilità
            if prev.ollama_available && !current.ollama_available {
                changes.push(HealthChange::OllamaDown);
            } else if !prev.ollama_available && current.ollama_available {
                changes.push(HealthChange::OllamaRestored);
            }
        }

        changes
    }

    // === Varianti macOS per i check di sistema ===

    /// Controlla la connettività di rete con un ping.
    #[cfg(target_os = "macos")]
    async fn check_network() -> NetworkState {
        // Su macOS il flag timeout è -W (millisecondi) e non secondi
        let result = Command::new("ping")
            .args(["-c", "1", "-W", "2000", "1.1.1.1"])
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => NetworkState::Connected,
            Ok(_) => NetworkState::Disconnected,
            Err(_) => NetworkState::Unknown,
        }
    }

    /// Controlla la connettività di rete — variante Linux.
    #[cfg(not(target_os = "macos"))]
    async fn check_network() -> NetworkState {
        let result = Command::new("ping")
            .args(["-c", "1", "-W", "2", "1.1.1.1"])
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => NetworkState::Connected,
            Ok(_) => NetworkState::Disconnected,
            Err(_) => NetworkState::Unknown,
        }
    }

    /// Controlla l'uso del disco — variante macOS (df senza --output).
    #[cfg(target_os = "macos")]
    async fn check_disk() -> Option<u8> {
        let output = Command::new("df")
            .args(["-h", "/"])
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Su macOS, df -h: la colonna "Capacity" è la 5a (indice 4), es. "45%"
        stdout.lines()
            .nth(1)
            .and_then(|line| {
                line.split_whitespace()
                    .nth(4)
                    .and_then(|s| s.trim_end_matches('%').parse().ok())
            })
    }

    /// Controlla l'uso del disco — variante Linux.
    #[cfg(not(target_os = "macos"))]
    async fn check_disk() -> Option<u8> {
        let output = Command::new("df")
            .args(["--output=pcent", "/"])
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines()
            .nth(1)
            .and_then(|line| line.trim().trim_end_matches('%').parse().ok())
    }

    /// Controlla il carico medio — variante macOS (sysctl).
    #[cfg(target_os = "macos")]
    async fn check_load() -> Option<f64> {
        let output = Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Output: "{ 1.42 1.35 1.28 }" — prendiamo il primo valore
        stdout.trim().trim_start_matches('{').trim()
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
    }

    /// Controlla il carico medio — variante Linux.
    #[cfg(not(target_os = "macos"))]
    async fn check_load() -> Option<f64> {
        let output = Command::new("cat")
            .arg("/proc/loadavg")
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
    }

    /// Controlla la memoria disponibile — variante macOS (vm_stat).
    #[cfg(target_os = "macos")]
    async fn check_memory() -> Option<u64> {
        let output = Command::new("vm_stat")
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut free_pages: u64 = 0;
        let mut speculative_pages: u64 = 0;
        let page_size: u64 = 16384; // Apple Silicon usa pagine da 16KB

        for line in stdout.lines() {
            if line.starts_with("Pages free:") {
                free_pages = line.split(':').nth(1)
                    .and_then(|s| s.trim().trim_end_matches('.').parse().ok())
                    .unwrap_or(0);
            }
            if line.starts_with("Pages speculative:") {
                speculative_pages = line.split(':').nth(1)
                    .and_then(|s| s.trim().trim_end_matches('.').parse().ok())
                    .unwrap_or(0);
            }
        }

        let available_mb = (free_pages + speculative_pages) * page_size / (1024 * 1024);
        Some(available_mb)
    }

    /// Controlla la memoria disponibile — variante Linux.
    #[cfg(not(target_os = "macos"))]
    async fn check_memory() -> Option<u64> {
        let output = Command::new("cat")
            .arg("/proc/meminfo")
            .output()
            .await
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.starts_with("MemAvailable:") {
                return line.split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|kb| kb / 1024);  // KB → MB
            }
        }
        None
    }

    /// Controlla se Ollama è raggiungibile tramite comando curl (evita dipendenza reqwest obbligatoria).
    async fn check_ollama(&self) -> bool {
        let result = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--connect-timeout", "3", &self.ollama_url])
            .output()
            .await;

        match result {
            Ok(output) => {
                let code = String::from_utf8_lossy(&output.stdout);
                code.trim().starts_with('2')
            }
            Err(_) => false,
        }
    }

    /// Tenta di correggere automaticamente un problema rilevato.
    /// Restituisce un messaggio descrittivo se l'azione è stata intrapresa.
    pub async fn attempt_fix(&self, change: &HealthChange) -> Option<String> {
        match change {
            HealthChange::DiskSpaceLow(_pct) => {
                // Prova a pulire file temporanei e cache
                info!("Self-healing: tentativo pulizia disco");
                let _ = Command::new("sh")
                    .args(["-c", "rm -rf /tmp/agentd-data/tmp_* 2>/dev/null; find /tmp -maxdepth 1 -name '*.tmp' -mtime +1 -delete 2>/dev/null"])
                    .output()
                    .await;
                Some("Ho provato a pulire file temporanei per liberare spazio disco.".to_string())
            }
            HealthChange::OllamaDown => {
                // Su macOS prova a riavviare Ollama
                info!("Self-healing: tentativo riavvio Ollama");
                let result = Command::new("sh")
                    .args(["-c", "pgrep -x ollama > /dev/null || ollama serve &"])
                    .output()
                    .await;
                match result {
                    Ok(output) if output.status.success() => {
                        Some("Ho provato a riavviare Ollama.".to_string())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Tipo di cambiamento rilevato.
#[derive(Debug, Clone, PartialEq)]
pub enum HealthChange {
    /// La rete è caduta
    NetworkDown,
    /// La rete è stata ripristinata
    NetworkRestored,
    /// Spazio disco basso (percentuale usata)
    DiskSpaceLow(u8),
    /// Memoria bassa (MB disponibili)
    MemoryLow(u64),
    /// Ollama non raggiungibile
    OllamaDown,
    /// Ollama ripristinato
    OllamaRestored,
}

impl HealthChange {
    /// Messaggio per l'utente in italiano.
    pub fn to_notification(&self) -> (String, String, agentos_common::types::Urgency) {
        use agentos_common::types::Urgency;
        match self {
            Self::NetworkDown => (
                "Rete disconnessa".into(),
                "La connessione di rete è caduta. Alcune funzionalità potrebbero non essere disponibili.".into(),
                Urgency::High,
            ),
            Self::NetworkRestored => (
                "Rete ripristinata".into(),
                "La connessione di rete è stata ripristinata.".into(),
                Urgency::Normal,
            ),
            Self::DiskSpaceLow(pct) => (
                "Spazio disco in esaurimento".into(),
                format!("Il disco è pieno al {}%. Considera di liberare spazio.", pct),
                Urgency::High,
            ),
            Self::MemoryLow(mb) => (
                "Memoria in esaurimento".into(),
                format!("Solo {} MB di RAM disponibili.", mb),
                Urgency::High,
            ),
            Self::OllamaDown => (
                "Ollama non disponibile".into(),
                "Il modello LLM locale non è raggiungibile. L'agente userà i backend remoti.".into(),
                Urgency::Normal,
            ),
            Self::OllamaRestored => (
                "Ollama disponibile".into(),
                "Il modello LLM locale è di nuovo raggiungibile.".into(),
                Urgency::Low,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_change_notification() {
        let (title, body, urgency) = HealthChange::NetworkDown.to_notification();
        assert!(title.contains("Rete"));
        assert!(!body.is_empty());
        assert_eq!(urgency, agentos_common::types::Urgency::High);
    }

    #[test]
    fn test_detect_network_change() {
        let mut monitor = HealthMonitor::new("http://localhost:11434");

        // Primo check — nessun cambiamento (nessuno stato precedente)
        let prev = SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: Some(50),
            load_avg: Some(1.0),
            mem_available_mb: Some(4000),
            ollama_available: true,
        };
        monitor.last_state = Some(prev);

        // Secondo check — rete caduta
        let current = SystemHealth {
            network: NetworkState::Disconnected,
            disk_usage_pct: Some(50),
            load_avg: Some(1.0),
            mem_available_mb: Some(4000),
            ollama_available: true,
        };

        let changes = monitor.detect_changes(&current);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], HealthChange::NetworkDown);
    }

    #[test]
    fn test_detect_disk_warning() {
        let mut monitor = HealthMonitor::new("http://localhost:11434");

        monitor.last_state = Some(SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: Some(80),
            load_avg: None,
            mem_available_mb: None,
            ollama_available: true,
        });

        let current = SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: Some(90),
            load_avg: None,
            mem_available_mb: None,
            ollama_available: true,
        };

        let changes = monitor.detect_changes(&current);
        assert!(changes.contains(&HealthChange::DiskSpaceLow(90)));
    }

    #[test]
    fn test_detect_memory_warning() {
        let mut monitor = HealthMonitor::new("http://localhost:11434");

        monitor.last_state = Some(SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: None,
            load_avg: None,
            mem_available_mb: Some(1000),
            ollama_available: true,
        });

        let current = SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: None,
            load_avg: None,
            mem_available_mb: Some(300),
            ollama_available: true,
        };

        let changes = monitor.detect_changes(&current);
        assert!(changes.contains(&HealthChange::MemoryLow(300)));
    }

    #[test]
    fn test_no_changes() {
        let mut monitor = HealthMonitor::new("http://localhost:11434");

        let state = SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: Some(50),
            load_avg: Some(1.0),
            mem_available_mb: Some(4000),
            ollama_available: true,
        };
        monitor.last_state = Some(state.clone());

        let changes = monitor.detect_changes(&state);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_ollama_change() {
        let mut monitor = HealthMonitor::new("http://localhost:11434");

        monitor.last_state = Some(SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: None,
            load_avg: None,
            mem_available_mb: None,
            ollama_available: true,
        });

        let current = SystemHealth {
            network: NetworkState::Connected,
            disk_usage_pct: None,
            load_avg: None,
            mem_available_mb: None,
            ollama_available: false,
        };

        let changes = monitor.detect_changes(&current);
        assert!(changes.contains(&HealthChange::OllamaDown));
    }
}
