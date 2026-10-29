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
