// SPDX-License-Identifier: MPL-2.0

//! **The coding-agent CLI cards: the Agent's half.**
//!
//! `koru-cli-updates` checks the installed CLIs and runs their updaters; what
//! this module adds is the projection onto the wire and the answer to a
//! card's button. The same split as [`crate::update`]: [`report_for`] and
//! [`apply`] decide, and the controller does.
//!
//! **Why the Agent and not the window.** A machine has one set of CLIs and
//! one npm registry to ask; an update is one process to run. Three windows
//! checking three times and two of them racing to run `codex update` is the
//! failure the Agent exists to prevent, the same way it owns Koru's own
//! update — and a window that connects after the check still has to be told,
//! which [`crate::app`]'s `Hello` path does for this report exactly as it does
//! for the update report.

use koru_agent_wire::{AgentCliChoice, AgentCliReport, AgentCliRow, AgentCliStage};
use koru_cli_updates::{CliStage, Controller, Report};
use koru_session_protocol::ProviderKind;

/// Where the Agent keeps which card a person closed, beside its other state.
pub const STATE_FILE: &str = "cli-updates.json";

/// The wire's copy of the controller's report. Whole, like [`crate::update::report_for`].
pub fn report_for(report: &Report) -> AgentCliReport {
    AgentCliReport {
        checked: report.checked,
        clis: report
            .clis
            .iter()
            .map(|row| AgentCliRow {
                id: row.id.clone(),
                name: row.name.clone(),
                installed: row.installed.clone(),
                latest: row.latest.clone(),
                stage: match row.stage {
                    CliStage::UpToDate => AgentCliStage::UpToDate,
                    CliStage::Available => AgentCliStage::Available,
                    CliStage::Updating => AgentCliStage::Updating,
                    CliStage::Updated => AgentCliStage::Updated,
                    CliStage::Failed => AgentCliStage::Failed,
                },
                message: row.message.clone(),
                detail: row.detail.clone(),
            })
            .collect(),
    }
}

/// The managed agents whose CLI changed between two reports, each with the
/// card it came from: a row whose stage became *Updated* (an update Koru
/// ran), or whose installed version moved (an update made outside Koru,
/// seen by the daily check or *Check now*). The session host re-probes each
/// one, which also rebuilds its model list (agent-models D6).
///
/// A card maps to an agent when the CLI it updates is the program that
/// agent's managed route runs — today `opencode`, `kimi`, `grok` and
/// `codex`. Claude's route runs its own pinned wrapper and Antigravity's a
/// server identified by digest, so updating the `claude` CLI changes
/// neither; a new build of either arrives with a new Koru, which restarts
/// the host. With no checked report before this one there is nothing to
/// compare, so the first check after a start refreshes nothing.
pub fn refresh_targets(
    previous: Option<&AgentCliReport>,
    next: &AgentCliReport,
) -> Vec<(String, ProviderKind)> {
    let Some(previous) = previous.filter(|report| report.checked) else {
        return Vec::new();
    };
    let mut targets: Vec<(String, ProviderKind)> = Vec::new();
    for row in &next.clis {
        let Some(before) = previous.clis.iter().find(|before| before.id == row.id) else {
            continue;
        };
        let updated = row.stage == AgentCliStage::Updated && before.stage != AgentCliStage::Updated;
        if !updated && row.installed == before.installed {
            continue;
        }
        if let Some(kind) = managed_kind(&row.id)
            && !targets.iter().any(|(_, known)| *known == kind)
        {
            targets.push((row.id.clone(), kind));
        }
    }
    targets
}

/// The managed agent whose route runs the program card `cli` updates.
fn managed_kind(cli: &str) -> Option<ProviderKind> {
    let program = koru_cli_updates::catalogue::KNOWN
        .iter()
        .find(|known| known.id == cli)?
        .program;
    koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .find(|registration| registration.executable == program)?
        .kind
        .parse()
        .ok()
}

/// What the refreshing client may do: ask for a re-probe, nothing else.
pub fn refresh_offer() -> koru_session_protocol::ProtocolOffer {
    use koru_session_protocol::{
        CapabilityName, CapabilitySet, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ProtocolRange,
        capability,
    };
    use std::str::FromStr;

    koru_session_protocol::ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new([CapabilityName::from_str(
            capability::PROVIDER_READINESS,
        )
        .expect("built-in capability")]),
    }
}

/// Take a card's choice. `Err` is the wire's `(code, message)` for a
/// `Response::Failed` — refused rather than ignored, for the reason
/// [`crate::update::allows`] gives.
pub fn apply(controller: &Controller, choice: &AgentCliChoice) -> Result<(), (String, String)> {
    let outcome = match choice {
        AgentCliChoice::Update { id } => controller.update(id),
        AgentCliChoice::Dismiss { id } => controller.dismiss(id),
        AgentCliChoice::CheckNow => {
            controller.check();
            Ok(())
        }
    };
    outcome.map_err(|error| {
        let message = match error {
            koru_cli_updates::ChoiceError::NoSuchCli => {
                "that agent CLI is not one the last check found installed"
            }
            koru_cli_updates::ChoiceError::WrongStage => {
                "that card is not at a step this choice answers"
            }
        };
        (error.code().to_owned(), message.to_owned())
    })
}

#[cfg(test)]
mod tests;
