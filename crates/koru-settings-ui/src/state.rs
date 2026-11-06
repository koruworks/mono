// SPDX-License-Identifier: MPL-2.0

//! **What the surface is told, and what it can tell about itself.**
//!
//! Three kinds of thing live here. [`Availability`] is the packet's owed state:
//! whether the authority is answering at all. [`VoiceFacts`], [`RendererRow`]
//! and [`AiSlot`] are readings a host takes of the world — a model cache, a
//! `renderer.conf`, an AI provider — none of which this crate may open. And
//! [`Notice`] is what a control says when what it asked for is not what came
//! back.

use std::time::{Duration, Instant};

use koru_settings::{DictationInput, DictationLanguage, DictationTier};

/// Whether the settings authority is answering.
///
/// **The SP-6b deviation this packet owes.** After a window has loaded
/// Settings, Gateway loss leaves the last authoritative values visible and
/// read-only rather than blank, wrong, or quietly accepting edits it cannot
/// keep. See [`crate::SettingsSurface::poll`] for how it is inferred and what
/// the client would have to grow for the inference to become an observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Availability {
    /// The authority has answered, or has not yet been given a reason not to.
    #[default]
    Live,
    /// The authority is not answering. Rows draw dimmed with their last
    /// authoritative values and refuse edits, and the section says why.
    Down,
}

impl Availability {
    pub fn is_down(self) -> bool {
        self == Self::Down
    }
}

/// How long a patch may be outstanding before the surface calls the authority
/// down.
///
/// **Longer than a round trip and shorter than a person's patience.** The link
/// re-reads the section after a refused write, so a live authority publishes
/// *something* — the value it kept — well inside this. Nothing publishing at
/// all is the signal, and it is the only one the client offers today.
pub(crate) const PATCH_DEADLINE: Duration = Duration::from_secs(3);

/// How long the surface waits for its first authoritative section before it
/// concludes there is nobody to talk to.
///
/// Twice the authority's own startup budget, so a window that opened while the
/// Agent was still starting the Gateway is not called down for being early.
pub(crate) const FIRST_SECTION_DEADLINE: Duration =
    Duration::from_millis(koru_settings::STARTUP_WAIT_MILLIS * 2);

/// **What a control says when what it asked for is not what came back.**
///
/// Quiet, and beside the row rather than over it: a settings page that throws
/// a dialog because a preference did not persist is a settings page nobody
/// leaves open. The control has already been put back to the authoritative
/// value by the time this is drawn, which is the substance — the sentence only
/// explains a change the person can already see.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub(crate) text: String,
    pub(crate) at: Instant,
}

impl Notice {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// When the reconciliation happened. A host that wants the sentence to
    /// fade has what it needs; the surface itself does not fade it, because a
    /// notice that disappeared on a timer is a notice somebody can miss.
    pub fn at(&self) -> Instant {
        self.at
    }
}

/// **A reading of what this machine offers dictation**, taken by the host.
///
/// Every field is a sentence or a list the host resolved: the kill switch, the
/// verified model cache, the microphone the worker last saw, the size out of a
/// signed descriptor. None of it is derivable here, and a surface that tried
/// would have to link `koru-voice-models`, `koru-dictation` and the worker's
/// device enumeration — which is exactly the coupling that kept these rows
/// inside `apps/koru` until now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceFacts {
    /// Why there is nothing to configure, if there is nothing to configure.
    /// `Some` replaces every control with this one sentence.
    pub blocked: Option<String>,
    /// The languages the picker offers, *Detect automatically* first, each
    /// named in itself.
    pub languages: Vec<(DictationLanguage, String)>,
    /// The microphones, *System default* first, then what the worker saw, then
    /// a remembered device that is missing — kept precisely because it is
    /// missing.
    pub inputs: Vec<(DictationInput, String)>,
    /// *That microphone is not connected*, when there is one to say.
    pub input_note: Option<String>,
    /// The two tiers' labels, and the sentence each one earns.
    pub tiers: Vec<(DictationTier, String)>,
    pub tier_notes: Vec<(DictationTier, String)>,
    /// The Model row: what is installed, and the button beside it.
    pub model_line: String,
    pub model_action: String,
    /// Whether that button removes rather than installs. **Derived by the host
    /// from the verified cache**, never stored: a `bool` remembered across a
    /// change eventually answers *installed* for a directory another channel
    /// pruned (ADR-251).
    pub model_removes: bool,
    /// Whether a consent record exists to revoke. `false` draws no row —
    /// revoking a promise nobody made is a control with nothing to control.
    pub consent_granted: bool,
    /// What the revoke row says, and the word on its button.
    pub consent_line: String,
    pub consent_action: String,
    /// The dictation chord, printed by the host through `koru_keys::show_hint`
    /// so it goes quiet with every other guide hint.
    pub chord: Option<String>,
}

impl VoiceFacts {
    /// Whether the host has said anything at all about dictation. A host that
    /// has not — Koru AI, the Terminal — draws no section rather than an empty
    /// one.
    pub(crate) fn is_absent(&self) -> bool {
        self.blocked.is_none() && self.languages.is_empty() && self.model_line.is_empty()
    }

    /// The sentence the selected tier earns, or nothing if the host gave none.
    pub(crate) fn tier_note(&self, tier: DictationTier) -> &str {
        self.tier_notes
            .iter()
            .find(|(candidate, _)| *candidate == tier)
            .map_or("", |(_, note)| note.as_str())
    }
}

/// **The renderer passthrough, in the host's own vocabulary.**
///
/// `renderer.conf` is exempt from the settings authority (ADR-266): it is
/// machine-survival state read before any service exists. The surface draws the
/// row and never learns what a `RendererBackend` is — the tokens are opaque
/// strings the host minted and the host reads back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RendererRow {
    pub label: String,
    /// *Applies the next time Koru starts.*
    pub note: String,
    /// `(token, label)`, in the order the row offers them.
    pub options: Vec<(String, String)>,
    pub current: String,
}

impl RendererRow {
    /// A host that offers no choice — macOS has one backend, so the row would
    /// be a control with nothing to control.
    pub fn none() -> Self {
        Self::default()
    }

    pub(crate) fn is_offered(&self) -> bool {
        !self.options.is_empty()
    }
}

/// **The AI section's body, filled by the host.**
///
/// The provider rows live in `koru-ai-ui`, where they are private methods on
/// `ChatSurface` drawn at fixed offsets into that surface's own viewport, with
/// their effects going down a private channel — there is no public seam to
/// compose and no way to give them a rect. Rather than reimplement them here
/// (which would move where an AI provider is configured, and this packet
/// changes no behavior beyond the re-homing), the section is a slot: the host
/// supplies the sentence and the way in.
///
/// **Not drawn for now.** The Koru AI card is off the System page; a host may
/// still fill the slot, and the surface keeps it for when the card returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiSlot {
    /// What the host wants to say about the configured provider.
    pub note: String,
    /// The way in. `None` draws the note alone.
    pub action: Option<String>,
}

impl AiSlot {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_empty(&self) -> bool {
        self.note.is_empty() && self.action.is_none()
    }
}

/// **The spelling dictionary the check would use is not on this device.**
///
/// The host reads the disk and the download's progress; the surface draws the
/// sentence and, when there is something to press, the button — which raises
/// [`Request::DownloadDictionary`](crate::Request::DownloadDictionary).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DictionaryRow {
    /// What is missing, or how far the download has got, or why it stopped.
    pub line: String,
    /// *Download* or *Try again*; `None` while a download runs.
    pub action: Option<String>,
}

/// **One coding agent on *Agent providers***, as the host read it: which
/// agent, whether its CLI is ready, where its setup guide is, and whether it
/// is on. The host owns every part of it — the agent list, the probe, the
/// stored switch — and this crate only draws it and says what was pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProviderRow {
    /// The agent's key — `claude`, `codex`, … — which names its brand mark
    /// and what its switch turns off.
    pub key: String,
    /// Its brand name, the same in every language.
    pub name: String,
    pub status: AgentProviderStatus,
    /// Its install or setup page, opened by *Setup guide ↗*.
    pub guide: Option<&'static str>,
    pub on: bool,
    /// Whether the row offers *Install*: the host knows the vendor's install
    /// command for what `status` says is missing, and runs it in a terminal
    /// tab when pressed ([`Request::InstallAgent`](crate::Request::InstallAgent)).
    pub install: bool,
}

/// **Where a new chat starts**, as the host's settings hold it: the
/// *Default permission for new chats* card. The host owns the stored values;
/// a press here moves the control at once and asks the host to store it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentDefaults {
    /// What a new chat with a supported agent (one with an auto mode)
    /// starts in.
    pub permission: koru_settings::AgentPermission,
    /// What a new chat with any other agent starts in: *Manual* or *Yolo*.
    pub permission_other: koru_settings::AgentPermission,
    /// The agent a new chat starts with, by key; `None` is *Auto*.
    pub agent: Option<String>,
    /// Why the chosen agent is not what a new chat gets — it is turned off,
    /// or not installed — in the host's words; `None` while it is.
    pub agent_note: Option<String>,
}

/// Whether an agent's CLI is ready, in the words the row says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentProviderStatus {
    /// Not read yet, or being read again.
    Checking,
    Ready,
    /// Its program is not on `PATH`; the row names it.
    NotInstalled {
        program: String,
    },
    /// Installed, and something Koru already knows stands in the way.
    NeedsAttention(AgentAttention),
}

/// Why an installed agent needs attention — only what Koru's existing
/// checks already know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentAttention {
    /// A version Koru's managed route does not accept yet; `update` is the
    /// command that updates it, named on the row when the agent has one,
    /// and `desktop_app` the agent's desktop app, whose own copy updates
    /// with the app — said after the command.
    UnsupportedVersion {
        update: Option<String>,
        desktop_app: Option<String>,
    },
    /// Signed out; `sign_in` is the command that signs it in, when known.
    SignedOut { sign_in: Option<String> },
    /// The check itself failed.
    CheckFailed,
    /// The agent is installed but the program a chat runs it through is
    /// not — Claude's `claude-agent-acp`, Antigravity's `agy_acp_server`.
    AdapterMissing { program: String },
}

impl AgentProviderStatus {
    /// The row's sentence: the status, and for one that is not ready, why.
    pub fn words(&self, lang: koru_i18n::Lang) -> String {
        use koru_i18n::{Key, fill, t};
        let detail = |status: Key, why: String| {
            fill(
                t(lang, Key::SettingsAgentStatusDetail),
                &[t(lang, status), &why],
            )
        };
        let not_on_path = |program: &str| fill(t(lang, Key::SettingsAgentNotOnPath), &[program]);
        match self {
            Self::Checking => t(lang, Key::TerminalChecking).to_owned(),
            Self::Ready => t(lang, Key::SettingsAgentReady).to_owned(),
            Self::NotInstalled { program } => {
                detail(Key::TerminalNotInstalled, not_on_path(program))
            }
            Self::NeedsAttention(why) => detail(
                Key::SettingsAgentNeedsAttention,
                match why {
                    AgentAttention::UnsupportedVersion { update: None, .. } => {
                        t(lang, Key::SettingsAgentUnsupportedVersion).to_owned()
                    }
                    AgentAttention::UnsupportedVersion {
                        update: Some(command),
                        desktop_app: None,
                    } => fill(
                        t(lang, Key::SettingsAgentUnsupportedVersionUpdate),
                        &[command],
                    ),
                    AgentAttention::UnsupportedVersion {
                        update: Some(command),
                        desktop_app: Some(app),
                    } => fill(
                        t(lang, Key::SettingsAgentUnsupportedVersionUpdateDesktopApp),
                        &[command, app],
                    ),
                    AgentAttention::SignedOut { sign_in: None } => {
                        t(lang, Key::SettingsAgentSignedOut).to_owned()
                    }
                    AgentAttention::SignedOut {
                        sign_in: Some(command),
                    } => fill(t(lang, Key::SettingsAgentSignedOutRun), &[command]),
                    AgentAttention::CheckFailed => {
                        t(lang, Key::SettingsAgentCheckFailed).to_owned()
                    }
                    AgentAttention::AdapterMissing { program } => not_on_path(program),
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests;
