// SPDX-License-Identifier: MPL-2.0

//! **What a host hands the model picker, and what it hands back**: plain
//! data rebuilt by the host for each call — the agents and what is known of
//! each, what is chosen now, what the choosing applies to, where the picker
//! opens — and the outcomes the host drains after it. Nothing here is a
//! service's type; the host maps its own onto these.

use std::fmt::{Debug, Display};
use std::hash::Hash;

use koru_draw::brand::ProfileMark;
use koru_draw::geometry::Rect;
use koru_i18n::{Key, Lang, t};

use super::FavoriteModel;
use crate::effort::{Effort, Levels};

/// **An agent's own default model**: the id an agent lists its *Default*
/// under, which names no model of its own — the agent decides when a
/// session starts.
pub const DEFAULT_MODEL: &str = "default";

/// What the control needs of a host's agent id: to compare, hash, copy and
/// write it out. A chat's is its provider id; a standalone host's can be a
/// `String`.
pub trait AgentId: Clone + Eq + Hash + Debug + Display {}

impl<T: Clone + Eq + Hash + Debug + Display> AgentId for T {}

/// **How ready one agent is**, as the rail and the list say it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    Checking,
    Ready,
    NotInstalled,
    /// Installed, but the program its conversations run through is not.
    AdapterMissing,
    SignedOut,
    UnsupportedVersion,
    CheckFailed,
    Unavailable,
}

impl Readiness {
    /// The word beside an agent that is not simply ready; `None` when it is.
    pub fn word(self, lang: Lang) -> Option<String> {
        let key = match self {
            Self::Ready => return None,
            Self::Checking => Key::TerminalChecking,
            Self::NotInstalled => Key::TerminalNotInstalled,
            Self::AdapterMissing => Key::SettingsAgentNeedsAttention,
            Self::SignedOut => Key::TerminalManagedSignedOut,
            Self::UnsupportedVersion => Key::AiAgentUnsupportedVersion,
            Self::CheckFailed => Key::TerminalManagedCheckFailed,
            Self::Unavailable => Key::AiAgentUnavailable,
        };
        Some(t(lang, key).to_owned())
    }
}

/// One model an agent lists, borrowed from the host's catalog.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelRecord<'a> {
    /// The id the agent takes; [`DEFAULT_MODEL`] is its *Default*.
    pub id: &'a str,
    pub group: &'a str,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub group_name: Option<&'a str>,
    /// The agent marks it as older: it waits behind *Legacy models*.
    pub legacy: bool,
    /// The id that runs it with a longer context window, if it has one.
    pub context: Option<&'a str>,
}

/// What is known of an agent's models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing<'a> {
    /// Not known yet, or on its way.
    Loading,
    Failed,
    Listed(Vec<ModelRecord<'a>>),
}

/// One agent the host offers, in the person's order.
#[derive(Clone, Debug)]
pub struct Agent<'a, P> {
    pub id: P,
    /// The key favorites name it by (`claude`); `None` keeps it out of
    /// *Favorites*.
    pub key: Option<&'a str>,
    pub label: &'a str,
    pub mark: ProfileMark,
    /// `None` until the host has heard how ready it is.
    pub readiness: Option<Readiness>,
    /// Turned off by the person: off the rail, unless it is the current one.
    pub hidden: bool,
    pub listing: Listing<'a>,
}

/// One of a built-in provider's models: its id and the words its row reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinModel {
    pub id: String,
    pub label: String,
}

/// **A begun conversation's own built-in provider** (S1): not an agent, so
/// it leads the rail with its flat list of models.
#[derive(Clone, Debug)]
pub struct Builtin<P> {
    pub id: P,
    pub name: String,
    pub mark: ProfileMark,
    pub models: Vec<BuiltinModel>,
}

/// What is chosen now.
#[derive(Copy, Clone, Debug)]
pub struct Current<'a, P> {
    pub provider: &'a P,
    /// The model id chosen, [`DEFAULT_MODEL`] for the agent's own.
    pub model: &'a str,
    pub effort: Effort,
}

/// **What the choosing applies to.**
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Applies {
    /// An ordinary conversation: free until its first message; then it
    /// keeps its provider (`begun`), an agent's (`agent_begun`), and model
    /// and effort change only where its host takes them per message.
    Conversation {
        begun: bool,
        agent_begun: bool,
        model_locked: bool,
        effort_locked: bool,
    },
    /// The next task of something that runs tasks (a Bot): nothing is
    /// begun and nothing is locked.
    NextTask,
}

impl Applies {
    pub fn begun(self) -> bool {
        matches!(self, Self::Conversation { begun: true, .. })
    }

    pub fn agent_begun(self) -> bool {
        matches!(
            self,
            Self::Conversation {
                agent_begun: true,
                ..
            }
        )
    }

    pub fn model_locked(self) -> bool {
        matches!(
            self,
            Self::Conversation {
                model_locked: true,
                ..
            }
        )
    }

    pub fn effort_locked(self) -> bool {
        matches!(
            self,
            Self::Conversation {
                effort_locked: true,
                ..
            }
        )
    }
}

/// The current model's longer context window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub tokens: u32,
    /// The longer window is the one chosen.
    pub long: bool,
}

/// **What the footer offers for the current model**: its levels, its Fast
/// mode (and whether it is on), its longer window.
#[derive(Clone, Debug, Default)]
pub struct FooterOffer {
    pub levels: Option<Levels>,
    pub fast: Option<bool>,
    pub context: Option<Context>,
}

/// Where the picker opens: the control that opened it, the viewport it
/// keeps inside, and the width of the composer it is as wide as.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Place {
    pub anchor: Rect,
    pub viewport: Rect,
    pub width: f32,
}

/// **Everything the picker reads**, rebuilt by the host for each call.
pub struct Inputs<'a, P> {
    pub lang: Lang,
    /// The agents, in the person's order.
    pub agents: Vec<Agent<'a, P>>,
    pub builtin: Option<Builtin<P>>,
    pub current: Option<Current<'a, P>>,
    pub applies: Applies,
    /// Only this agent may be offered (an owned conversation's, a Bot's):
    /// the rail is hidden and holds it alone.
    pub allowed: Option<&'a P>,
    /// The agent the rail opens on when nothing else decides.
    pub preferred: Option<P>,
    pub favorites: &'a [FavoriteModel],
    /// The host can open its agent settings: the rail's **+** and
    /// *Manage agent providers* under an agent that cannot run.
    pub manage: bool,
    pub footer: FooterOffer,
    pub place: Place,
}

impl<P: AgentId> Inputs<'_, P> {
    /// The agent `id`, while the host offers it.
    pub fn agent(&self, id: &P) -> Option<&Agent<'_, P>> {
        self.agents.iter().find(|agent| &agent.id == id)
    }

    /// The models listed for `id`, empty while none are known.
    pub fn models(&self, id: &P) -> &[ModelRecord<'_>] {
        match self.agent(id).map(|agent| &agent.listing) {
            Some(Listing::Listed(models)) => models,
            _ => &[],
        }
    }

    pub(crate) fn readiness(&self, id: &P) -> Option<Readiness> {
        self.agent(id).and_then(|agent| agent.readiness)
    }

    pub(crate) fn key(&self, id: &P) -> Option<&str> {
        self.agent(id).and_then(|agent| agent.key)
    }
}

/// **What the person did**, for the host to carry out, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<P> {
    /// An agent's model ([`DEFAULT_MODEL`] for its *Default*), or a
    /// favorite, or the longer window's id.
    Model { provider: P, model: String },
    /// One of the built-in provider's models.
    BuiltinModel(String),
    /// A level pressed or stepped to on the strip.
    Level(Effort),
    /// The strip was pressed while the level only shows.
    EffortLocked,
    /// Fast mode, on or off.
    Fast(bool),
    /// The context pill: the longer window wanted, or the model's own.
    Context(bool),
    /// Open the host's agent settings.
    Setup,
    /// A ☆ pressed: the whole list, to store.
    Favorites(Vec<FavoriteModel>),
    /// An agent's models are wanted and not listed: ask for them.
    Catalog(P),
    /// ↻: rebuild an agent's list now. Say [`super::PickerState::refresh_started`]
    /// once it is asked for.
    Refresh(P),
}

impl<P> Outcome<P> {
    /// The host's answer to this outcome is the answer of the call that
    /// made it — whether the choice was taken.
    pub fn answers(&self) -> bool {
        matches!(
            self,
            Self::Model { .. }
                | Self::BuiltinModel(_)
                | Self::Level(_)
                | Self::Fast(_)
                | Self::Context(_)
        )
    }
}

/// A model's label: the agent's own name for it, or the id formatted.
pub fn model_label(model: &ModelRecord<'_>) -> String {
    model
        .name
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_label(model.id, model.group))
}

/// An agent's models other than its own *Default* entry.
pub fn listed_models<'m, 'a>(
    models: &'m [ModelRecord<'a>],
) -> impl Iterator<Item = &'m ModelRecord<'a>> + 'm {
    models.iter().filter(|model| model.id != DEFAULT_MODEL)
}

/// The groups an agent's models fall into, in the order first seen.
pub fn model_groups(models: &[ModelRecord<'_>]) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for model in listed_models(models) {
        if !groups.iter().any(|group| group == model.group) {
            groups.push(model.group.to_owned());
        }
    }
    groups
}

/// A group's heading: the agent's own name for it, or the id formatted.
pub fn group_label(models: &[ModelRecord<'_>], group: &str) -> String {
    models
        .iter()
        .filter(|model| model.group == group)
        .find_map(|model| model.group_name.map(str::to_owned))
        .unwrap_or_else(|| fallback_label(group, ""))
}

/// The *Default* row's label: the agent's own default entry's name when it
/// lists one, otherwise Koru's word.
pub fn default_model_label(models: &[ModelRecord<'_>], lang: Lang) -> String {
    models
        .iter()
        .find(|model| model.id == DEFAULT_MODEL)
        .and_then(|model| model.name.map(str::to_owned))
        .unwrap_or_else(|| t(lang, Key::AiModelAgentDefault).to_owned())
}

/// **The listed model an id names**, and whether the id is that model's
/// longer context.
pub fn find_listed<'m, 'a>(
    models: &'m [ModelRecord<'a>],
    id: &str,
) -> Option<(&'m ModelRecord<'a>, bool)> {
    models.iter().find_map(|model| {
        if model.id == id {
            Some((model, false))
        } else if model.context == Some(id) {
            Some((model, true))
        } else {
            None
        }
    })
}

/// A model id as a person would write it, when the agent sends no name. A
/// formatting rule rather than a table, so it never goes stale: the group
/// prefix (or else everything up to the last `/`) goes, `-`, `_` and spaces
/// split words, a date (`2025-01-31`) stays whole, runs of one- and
/// two-digit numbers join with `.`, and each word that starts with a letter
/// is capitalised. `claude-opus-4-1` reads `Claude Opus 4.1`;
/// `o3-mini-2025-01-31` reads `O3 Mini 2025-01-31`; `deepseek/v4-pro` under
/// `deepseek` reads `V4 Pro`.
pub fn fallback_label(id: &str, group: &str) -> String {
    let tail = id
        .strip_prefix(&format!("{group}/"))
        .filter(|_| !group.is_empty())
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id));
    let digits = |token: &str, lengths: std::ops::RangeInclusive<usize>| {
        lengths.contains(&token.len()) && token.bytes().all(|b| b.is_ascii_digit())
    };
    let raw: Vec<&str> = tail
        .split(['-', '_', ' '])
        .filter(|token| !token.is_empty())
        .collect();
    // A year, a month and a day in a row are one date, not a year and a
    // version number.
    let mut tokens: Vec<String> = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        if let [year, month, day, ..] = raw[index..]
            && digits(year, 4..=4)
            && digits(month, 2..=2)
            && digits(day, 2..=2)
        {
            tokens.push(format!("{year}-{month}-{day}"));
            index += 3;
        } else {
            tokens.push(raw[index].to_owned());
            index += 1;
        }
    }
    let mut words: Vec<String> = Vec::new();
    let mut previous_short_number = false;
    for token in &tokens {
        let token = token.as_str();
        let number = digits(token, 1..=2);
        match words.last_mut() {
            Some(last) if number && previous_short_number => {
                last.push('.');
                last.push_str(token);
            }
            _ => {
                let mut chars = token.chars();
                words.push(match chars.next() {
                    Some(first) if first.is_alphabetic() => {
                        first.to_uppercase().chain(chars).collect()
                    }
                    _ => token.to_owned(),
                });
            }
        }
        previous_short_number = number;
    }
    if words.is_empty() {
        id.to_owned()
    } else {
        words.join(" ")
    }
}
