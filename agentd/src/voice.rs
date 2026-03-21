//! Voice Input — trascrizione audio tramite Whisper.
//!
//! Workflow: registra audio → salva file temp → Whisper trascrive → testo
//! Su macOS usa `sox` (rec) per la registrazione, con fallback a dispositivi nativi.
//! Per la trascrizione: whisper.cpp, oppure OpenAI Whisper API.

use anyhow::{Result, anyhow};
use tokio::process::Command;
use tracing::{info, debug, warn};
use std::path::Path;

/// Backend per la trascrizione vocale.
#[derive(Debug, Clone)]
pub enum WhisperBackend {
    /// whisper.cpp CLI (whisper-cli o main)
    WhisperCpp { binary: String, model: String },
    /// API Ollama con modello whisper (se disponibile)
    Ollama { url: String },
    /// API OpenAI Whisper (richiede API key)
    OpenAiWhisper { api_key: String },
}

/// Modulo di input vocale.
pub struct VoiceInput {
    backend: WhisperBackend,
    /// Directory temporanea per i file audio
    temp_dir: String,
}

impl VoiceInput {
    /// Crea un nuovo modulo di input vocale.
    pub fn new(backend: WhisperBackend) -> Self {
        let temp_dir = std::env::temp_dir()
            .join("agentos-voice")
            .to_str()
            .unwrap_or("/tmp/agentos-voice")
            .to_string();

        // Crea la directory temp se necessario
        let _ = std::fs::create_dir_all(&temp_dir);

        Self { backend, temp_dir }
    }

    /// Verifica quale registratore audio è disponibile su macOS.
    /// Restituisce il nome del comando trovato ("rec", "sox") oppure None.
    async fn detect_recorder() -> Option<String> {
        // Prima prova `rec` (parte di sox, interfaccia più semplice)
        if let Ok(output) = Command::new("which").arg("rec").output().await {
            if output.status.success() {
                return Some("rec".to_string());
            }
        }
        // Poi prova `sox` direttamente
        if let Ok(output) = Command::new("which").arg("sox").output().await {
            if output.status.success() {
                return Some("sox".to_string());
            }
        }
        // Su Linux, prova arecord (ALSA)
        if let Ok(output) = Command::new("which").arg("arecord").output().await {
            if output.status.success() {
                return Some("arecord".to_string());
            }
        }
        None
    }

    /// Registra audio dal microfono per N secondi. Restituisce il path del file.
    /// Su macOS usa `rec` (SoX) o `sox`. Su Linux usa `arecord` o `parecord`.
    pub async fn record(&self, duration_secs: u32) -> Result<String> {
        let output_path = format!("{}/recording.wav", self.temp_dir);

        info!(duration = duration_secs, "Registrazione audio...");

        let recorder = Self::detect_recorder().await
            .ok_or_else(|| anyhow!(
                "Nessun registratore audio trovato. Installa SoX: brew install sox (macOS) o apt install sox (Linux)"
            ))?;

        debug!(recorder = %recorder, "Registratore audio rilevato");

        let result = match recorder.as_str() {
            // `rec` è il modo più semplice su macOS (parte del pacchetto sox)
            "rec" => {
                Command::new("rec")
                    .args([
                        &output_path,
                        "rate", "16000",
                        "channels", "1",
                        "trim", "0", &duration_secs.to_string(),
                    ])
                    .output()
                    .await
            }
            // `sox` con input dal dispositivo di default
            "sox" => {
                Command::new("sox")
                    .args([
                        "-d",           // dispositivo audio di default
                        "-r", "16000",
                        "-c", "1",
                        "-b", "16",
                        &output_path,
                        "trim", "0", &duration_secs.to_string(),
                    ])
                    .output()
                    .await
            }
            // `arecord` su Linux (ALSA)
            "arecord" => {
                Command::new("arecord")
                    .args([
                        "-d", &duration_secs.to_string(),
                        "-f", "S16_LE",
                        "-r", "16000",
                        "-c", "1",
                        &output_path,
                    ])
                    .output()
                    .await
            }
            other => {
                return Err(anyhow!("Registratore non supportato: {}", other));
            }
        };

        match result {
            Ok(output) if output.status.success() => {
                info!(path = %output_path, "Audio registrato");
                Ok(output_path)
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(anyhow!("Errore registrazione con {}: {}", recorder, stderr))
            }
            Err(e) => {
                Err(anyhow!("Errore avvio registratore {}: {}", recorder, e))
            }
        }
    }

    /// Trascrive un file audio in testo.
    pub async fn transcribe(&self, audio_path: &str) -> Result<String> {
        if !Path::new(audio_path).exists() {
            return Err(anyhow!("File audio non trovato: {}", audio_path));
        }

        match &self.backend {
            WhisperBackend::WhisperCpp { binary, model } => {
                self.transcribe_whisper_cpp(audio_path, binary, model).await
            }
            WhisperBackend::Ollama { url } => {
                self.transcribe_ollama(audio_path, url).await
            }
            WhisperBackend::OpenAiWhisper { api_key } => {
                self.transcribe_openai(audio_path, api_key).await
            }
        }
    }

    /// Trascrizione con whisper.cpp.
    async fn transcribe_whisper_cpp(&self, audio_path: &str, binary: &str, model: &str) -> Result<String> {
        debug!(binary = binary, model = model, "Trascrizione con whisper.cpp");

        let output = Command::new(binary)
            .args([
                "-m", model,
                "-f", audio_path,
                "--language", "it",
                "--no-timestamps",
                "--output-txt",
            ])
            .output()
            .await
            .map_err(|e| anyhow!("whisper.cpp non disponibile: {}", e))?;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            info!(chars = text.len(), "Trascrizione completata");
            Ok(text)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow!("Errore whisper.cpp: {}", stderr))
        }
    }

    /// Trascrizione via Ollama (placeholder per quando sarà supportato).
    async fn transcribe_ollama(&self, _audio_path: &str, _url: &str) -> Result<String> {
        warn!("Trascrizione Ollama non ancora supportata");
        Err(anyhow!("Backend Ollama per trascrizione non ancora implementato"))
    }

    /// Trascrizione tramite API OpenAI Whisper.
    /// Invia il file audio all'endpoint /v1/audio/transcriptions.
    async fn transcribe_openai(&self, audio_path: &str, api_key: &str) -> Result<String> {
        debug!("Trascrizione con OpenAI Whisper API");

        // Usa curl per inviare il file audio multipart
        let output = Command::new("curl")
            .args([
                "-s",
                "--max-time", "60",
                "https://api.openai.com/v1/audio/transcriptions",
                "-H", &format!("Authorization: Bearer {}", api_key),
                "-F", &format!("file=@{}", audio_path),
                "-F", "model=whisper-1",
                "-F", "language=it",
                "-F", "response_format=text",
            ])
            .output()
            .await
            .map_err(|e| anyhow!("Errore chiamata OpenAI Whisper: {}", e))?;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                return Err(anyhow!("Trascrizione vuota da OpenAI Whisper"));
            }
            info!(chars = text.len(), "Trascrizione OpenAI completata");
            Ok(text)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(anyhow!("Errore OpenAI Whisper: {} {}", stderr, stdout))
        }
    }

    /// Usa macOS `say` per la sintesi vocale (TTS).
    /// Restituisce Ok(()) se il comando è stato eseguito con successo.
    pub async fn speak(text: &str) -> Result<()> {
        info!(text_len = text.len(), "Sintesi vocale (TTS)");

        // Su macOS usa il comando `say` nativo
        let result = Command::new("say")
            .args(["-v", "Alice", text])  // Alice è la voce italiana
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Prova senza specificare la voce (fallback alla voce di sistema)
                let fallback = Command::new("say")
                    .arg(text)
                    .output()
                    .await;
                match fallback {
                    Ok(o) if o.status.success() => Ok(()),
                    _ => Err(anyhow!("Errore sintesi vocale: {}", stderr)),
                }
            }
            Err(e) => Err(anyhow!(
                "Comando `say` non disponibile (solo macOS): {}", e
            )),
        }
    }

    /// Registra e trascrive in un unico passo (push-to-talk).
    pub async fn listen_and_transcribe(&self, duration_secs: u32) -> Result<String> {
        let audio_path = self.record(duration_secs).await?;
        let text = self.transcribe(&audio_path).await?;

        // Pulisci il file temporaneo
        let _ = std::fs::remove_file(&audio_path);

        Ok(text)
    }

    /// Rileva automaticamente il miglior backend Whisper disponibile.
    /// Ordine: whisper.cpp → OpenAI API key dall'env → Ollama.
    pub async fn detect_backend() -> WhisperBackend {
        // 1. Cerca whisper.cpp (whisper-cli o whisper-cpp)
        for binary in &["whisper-cli", "whisper-cpp", "whisper", "main"] {
            if let Ok(output) = Command::new("which").arg(binary).output().await {
                if output.status.success() {
                    // Cerca il modello di default
                    let model_paths = vec![
                        "/usr/local/share/whisper/ggml-base.bin",
                        "/opt/homebrew/share/whisper/ggml-base.bin",
                        "models/ggml-base.bin",
                    ];
                    let model = model_paths.iter()
                        .find(|p| Path::new(p).exists())
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "ggml-base.bin".to_string());

                    info!(binary = binary, model = %model, "whisper.cpp rilevato");
                    return WhisperBackend::WhisperCpp {
                        binary: binary.to_string(),
                        model,
                    };
                }
            }
        }

        // 2. API key OpenAI dall'ambiente
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            if !api_key.is_empty() {
                info!("Backend OpenAI Whisper API selezionato");
                return WhisperBackend::OpenAiWhisper { api_key };
            }
        }

        // 3. Fallback a Ollama
        info!("Nessun backend Whisper rilevato — fallback a Ollama");
        WhisperBackend::Ollama {
            url: "http://localhost:11434".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_input_creation() {
        let backend = WhisperBackend::WhisperCpp {
            binary: "whisper-cli".into(),
            model: "/usr/share/whisper/ggml-base.bin".into(),
        };
        let _voice = VoiceInput::new(backend);
    }

    #[test]
    fn test_whisper_backend_debug() {
        let backend = WhisperBackend::WhisperCpp {
            binary: "whisper".into(),
            model: "base".into(),
        };
        let debug_str = format!("{:?}", backend);
        assert!(debug_str.contains("WhisperCpp"));
    }

    #[test]
    fn test_openai_whisper_backend_debug() {
        let backend = WhisperBackend::OpenAiWhisper {
            api_key: "sk-test".into(),
        };
        let debug_str = format!("{:?}", backend);
        assert!(debug_str.contains("OpenAiWhisper"));
    }

    #[tokio::test]
    async fn test_transcribe_missing_file() {
        let backend = WhisperBackend::WhisperCpp {
            binary: "whisper".into(),
            model: "base".into(),
        };
        let voice = VoiceInput::new(backend);
        let result = voice.transcribe("/nonexistent/audio.wav").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_detect_recorder() {
        // Verifica che il rilevamento non faccia panic
        let _recorder = VoiceInput::detect_recorder().await;
    }

    #[tokio::test]
    async fn test_detect_backend() {
        // Verifica che il rilevamento automatico funzioni senza panic
        let _backend = VoiceInput::detect_backend().await;
    }
}
