// SPDX-License-Identifier: MPL-2.0

//! What each CLI's card says — the decisions, with no I/O in them.
//!
//! A row is a function of three facts the controller holds per CLI: what the
//! last check found ([`Status`]), what an update run is doing or did
//! ([`Activity`]), and which offer the person already closed. Keeping that a
//! function is what lets every branch be asserted without a registry, a
//! binary or a thread.

use crate::Version;
use crate::catalogue::KnownCli;

/// The last check's answer for one installed CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub cli: &'static KnownCli,
    /// `None` when the program resolved but printed no version.
    pub installed: Option<Version>,
    /// What the channel offers, or why that could not be read — a registry
    /// that was down, a `grok update --check` that errored. The error is
    /// developer text for a log; it never reaches a card.
    pub latest: Result<Version, String>,
}

/// What an update run is doing for one CLI, independent of the last check.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Activity {
    #[default]
    Idle,
    Updating {
        /// The updater's most recent line of output.
        last_line: Option<String>,
    },
    /// The run finished and the CLI reports this version now.
    Updated { version: Version },
    Failed {
        /// The updater's last line — its own words about what went wrong.
        message: Option<String>,
        /// Exit status and the retained output tail, for a log.
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliStage {
    UpToDate,
    Available,
    Updating,
    Updated,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliReport {
    pub id: String,
    pub name: String,
    /// Empty when the program printed no version.
    pub installed: String,
    /// Empty when the channel could not be read.
    pub latest: String,
    pub stage: CliStage,
    /// A card's second line while updating or after a failure: the updater's
    /// own last words. `None` otherwise.
    pub message: Option<String>,
    /// Developer text: a check error, or a failed run's output tail.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Report {
    /// Whether a check has completed since the Agent started. `false` with an
    /// empty list means "not yet", not "nothing installed".
    pub checked: bool,
    /// One row per installed, known CLI, in catalogue order.
    pub clis: Vec<CliReport>,
}

/// The row for one CLI. `dismissed` is the version the person closed a card
/// about, if any.
pub fn row(status: &Status, activity: &Activity, dismissed: Option<&str>) -> CliReport {
    let mut report = CliReport {
        id: status.cli.id.to_owned(),
        name: status.cli.name().to_owned(),
        installed: status
            .installed
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        latest: status
            .latest
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        stage: CliStage::UpToDate,
        message: None,
        detail: status.latest.as_ref().err().cloned(),
    };
    match activity {
        Activity::Updating { last_line } => {
            report.stage = CliStage::Updating;
            report.message = last_line.clone();
        }
        Activity::Updated { version } => {
            report.stage = CliStage::Updated;
            report.installed = version.to_string();
        }
        Activity::Failed { message, detail } => {
            report.stage = CliStage::Failed;
            report.message = message.clone();
            report.detail = Some(detail.clone());
        }
        Activity::Idle => {
            if let (Some(installed), Ok(latest)) = (&status.installed, &status.latest)
                && latest > installed
                && dismissed != Some(latest.to_string().as_str())
            {
                report.stage = CliStage::Available;
            }
        }
    }
    report
}
