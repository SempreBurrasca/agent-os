//! Planner — pianifica l'esecuzione di un intento in una sequenza di passi.
//!
//! Riceve un ParsedIntent dall'Intent Engine e coordina il Guardian
//! e l'Executor per eseguire i comandi.

use agentos_common::types::{GuardianVerdict, ParsedIntent, RiskZone};
use tracing::debug;

/// Piano di esecuzione — sequenza di passi con i verdetti del Guardian.
#[derive(Debug)]
pub struct ExecutionPlan {
    /// L'intento originale
    pub intent: ParsedIntent,
    /// Verdetti del Guardian per ogni comando
    pub verdicts: Vec<GuardianVerdict>,
    /// Se il piano richiede conferma dall'utente
    pub needs_confirmation: bool,
    /// Se il piano è bloccato (almeno un comando in zona rossa)
    pub is_blocked: bool,
}

/// Il Planner crea piani di esecuzione.
pub struct Planner {
    /// Numero massimo di comandi in un piano
    max_steps: u32,
}

impl Planner {
    /// Crea un nuovo Planner.
    pub fn new(max_steps: u32) -> Self {
        Self { max_steps }
    }

    /// Crea un piano di esecuzione da un intento parsato.
    /// I verdetti vengono dal Guardian.
    pub fn create_plan(&self, intent: ParsedIntent, verdicts: Vec<GuardianVerdict>) -> ExecutionPlan {
        let is_blocked = verdicts.iter().any(|v| v.blocked);
        let needs_confirmation = verdicts.iter().any(|v| v.zone == RiskZone::Yellow);

        debug!(
            commands = intent.commands.len(),
            blocked = is_blocked,
            needs_confirmation = needs_confirmation,
            "Piano creato"
        );

        ExecutionPlan {
            intent,
            verdicts,
            needs_confirmation,
            is_blocked,
        }
    }

    /// Verifica se un intento ha troppi comandi.
    pub fn is_too_large(&self, intent: &ParsedIntent) -> bool {
        intent.commands.len() > self.max_steps as usize
    }

    /// Filtra i comandi bloccati dal piano, restituendo solo quelli eseguibili.
    pub fn executable_commands(plan: &ExecutionPlan) -> Vec<&str> {
        plan.intent.commands.iter()
            .zip(plan.verdicts.iter())
            .filter(|(_, v)| !v.blocked)
            .map(|(cmd, _)| cmd.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_plan_all_green() {
        let planner = Planner::new(10);
        let intent = ParsedIntent {
            understood: true,
            intent: "file_operation".to_string(),
            commands: vec!["ls -la".to_string(), "pwd".to_string()],
            explanation: "Mostro i file".to_string(),
            needs_interaction: false,
        };
        let verdicts = vec![
            GuardianVerdict { zone: RiskZone::Green, reason: "ok".to_string(), command: "ls -la".to_string(), blocked: false },
            GuardianVerdict { zone: RiskZone::Green, reason: "ok".to_string(), command: "pwd".to_string(), blocked: false },
        ];

        let plan = planner.create_plan(intent, verdicts);
        assert!(!plan.is_blocked);
        assert!(!plan.needs_confirmation);
    }

    #[test]
    fn test_create_plan_with_yellow() {
        let planner = Planner::new(10);
        let intent = ParsedIntent {
            understood: true,
            intent: "system_command".to_string(),
            commands: vec!["sudo apt update".to_string()],
            explanation: "Aggiorno i pacchetti".to_string(),
            needs_interaction: false,
        };
        let verdicts = vec![
            GuardianVerdict { zone: RiskZone::Yellow, reason: "sudo".to_string(), command: "sudo apt update".to_string(), blocked: false },
        ];

        let plan = planner.create_plan(intent, verdicts);
        assert!(!plan.is_blocked);
        assert!(plan.needs_confirmation);
    }

    #[test]
    fn test_create_plan_with_blocked() {
        let planner = Planner::new(10);
        let intent = ParsedIntent {
            understood: true,
            intent: "file_operation".to_string(),
            commands: vec!["rm -rf /".to_string()],
            explanation: "...".to_string(),
            needs_interaction: false,
        };
        let verdicts = vec![
            GuardianVerdict { zone: RiskZone::Red, reason: "distruttivo".to_string(), command: "rm -rf /".to_string(), blocked: true },
        ];

        let plan = planner.create_plan(intent, verdicts);
        assert!(plan.is_blocked);
    }

    #[test]
    fn test_executable_commands() {
        let planner = Planner::new(10);
        let intent = ParsedIntent {
            understood: true,
            intent: "mixed".to_string(),
            commands: vec!["ls".to_string(), "rm -rf /".to_string(), "pwd".to_string()],
            explanation: "".to_string(),
            needs_interaction: false,
        };
        let verdicts = vec![
            GuardianVerdict { zone: RiskZone::Green, reason: "ok".to_string(), command: "ls".to_string(), blocked: false },
            GuardianVerdict { zone: RiskZone::Red, reason: "no".to_string(), command: "rm -rf /".to_string(), blocked: true },
            GuardianVerdict { zone: RiskZone::Green, reason: "ok".to_string(), command: "pwd".to_string(), blocked: false },
        ];

        let plan = planner.create_plan(intent, verdicts);
        let executable = Planner::executable_commands(&plan);
        assert_eq!(executable, vec!["ls", "pwd"]);
    }

    #[test]
    fn test_is_too_large() {
        let planner = Planner::new(3);
        let small_intent = ParsedIntent {
            understood: true,
            intent: "test".to_string(),
            commands: vec!["a".to_string(), "b".to_string()],
            explanation: "".to_string(),
            needs_interaction: false,
        };
        assert!(!planner.is_too_large(&small_intent));

        let large_intent = ParsedIntent {
            understood: true,
            intent: "test".to_string(),
            commands: vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()],
            explanation: "".to_string(),
            needs_interaction: false,
        };
        assert!(planner.is_too_large(&large_intent));
    }
}
