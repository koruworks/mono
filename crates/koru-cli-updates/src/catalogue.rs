// SPDX-License-Identifier: MPL-2.0

//! The CLIs this crate knows how to check and how to update.
//!
//! Every row was verified against the installed CLI on 2026-10-27: its
//! `--version` output, where its latest version is published, and which
//! command updates it. A row is added only with all three, because the card a
//! row produces says "vX → vY" and offers a button, and both have to be true.
//!
//! **Not listed, and why:**
//! - `claude-agent-acp`, the wrapper Koru's managed Claude route runs. Koru
//!   pins its exact version and its provider note says to install that pin,
//!   so offering npm's latest would work against the route it exists for.
//! - `agy` (Antigravity) has an `update` verb but publishes no latest version
//!   anywhere this crate can read; a card could not say what it offers.
//! - `muse` (Meta's Muse Code) has no self-update and no package this crate
//!   could find; its Windows launcher only reports an unsupported platform.
//! - `gemini` is retired from the profile catalogue (ADR-391).

use koru_session_profiles::ProfileLabel;

/// Where a CLI's newest release is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatestSource {
    /// `https://registry.npmjs.org/<package>/latest`. Used for every
    /// npm-published CLI, including ones a person installed through a native
    /// installer whose releases share npm's version numbers (Claude Code).
    Npm(&'static str),
    /// `grok update --check --json`, which prints the installed and the latest
    /// version for the channel the installation follows. Grok is a native
    /// binary with its own channel and no npm package.
    GrokCheck,
}

/// How a CLI is brought to its newest release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateCommand {
    /// The CLI's own updater: the program, then these arguments. Preferred
    /// wherever it exists, because it knows how it was installed — a native
    /// Claude install and an npm one both answer to `claude update`.
    SelfUpdate(&'static [&'static str]),
    /// `npm install -g <package>@latest`, for a CLI that is only an npm
    /// package and has no updater of its own.
    NpmInstall(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownCli {
    /// Stable identity on the wire and in the dismissal file. The program
    /// name, which the profile catalogue also keys on.
    pub id: &'static str,
    /// The executable, resolved through `koru_session_profiles::resolve_executable`.
    pub program: &'static str,
    /// Which profile row this is, for the brand name every launch surface
    /// already prints.
    pub label: ProfileLabel,
    pub latest: LatestSource,
    pub update: UpdateCommand,
}

impl KnownCli {
    /// The name a card shows. Brand names are identical in every language.
    pub fn name(&self) -> &'static str {
        self.label.brand_name().unwrap_or(self.id)
    }
}

pub const KNOWN: &[KnownCli] = &[
    KnownCli {
        id: "claude",
        program: "claude",
        label: ProfileLabel::Claude,
        latest: LatestSource::Npm("@anthropic-ai/claude-code"),
        // `claude update` on a native install downloads the native build and on
        // a package-manager install defers to the package manager — its own
        // words in `--help`.
        update: UpdateCommand::SelfUpdate(&["update"]),
    },
    KnownCli {
        id: "codex",
        program: "codex",
        label: ProfileLabel::CodexCli,
        latest: LatestSource::Npm("@openai/codex"),
        update: UpdateCommand::SelfUpdate(&["update"]),
    },
    KnownCli {
        id: "grok",
        program: "grok",
        label: ProfileLabel::Grok,
        latest: LatestSource::GrokCheck,
        update: UpdateCommand::SelfUpdate(&["update"]),
    },
    KnownCli {
        id: "copilot",
        program: "copilot",
        label: ProfileLabel::Copilot,
        latest: LatestSource::Npm("@github/copilot"),
        update: UpdateCommand::SelfUpdate(&["update"]),
    },
    KnownCli {
        id: "kimi",
        program: "kimi",
        label: ProfileLabel::Kimi,
        latest: LatestSource::Npm("@moonshot-ai/kimi-code"),
        // `-y` answers the updater's own confirmation; the person already
        // pressed Update, and a prompt on a pipe nobody reads would hang.
        update: UpdateCommand::SelfUpdate(&["update", "-y"]),
    },
    KnownCli {
        id: "opencode",
        program: "opencode",
        label: ProfileLabel::OpenCode,
        latest: LatestSource::Npm("opencode-ai"),
        update: UpdateCommand::NpmInstall("opencode-ai"),
    },
    KnownCli {
        id: "qwen",
        program: "qwen",
        label: ProfileLabel::QwenCode,
        latest: LatestSource::Npm("@qwen-code/qwen-code"),
        update: UpdateCommand::NpmInstall("@qwen-code/qwen-code"),
    },
];

pub fn known(id: &str) -> Option<&'static KnownCli> {
    KNOWN.iter().find(|cli| cli.id == id)
}
