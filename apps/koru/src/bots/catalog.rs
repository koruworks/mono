// SPDX-License-Identifier: MPL-2.0

//! **The agents-and-models catalog for a Bot, on a thread of its own**
//! (Bots P1.8 S6).
//!
//! A Bot's create form and its settings choose an agent, a model, an effort,
//! a mode and Fast with the agent-configuration control
//! (`koru_ai_ui::agent_catalog::config`), which reads plain data. This is
//! where a Bot tab gets that data without a chat surface: which agent
//! runtimes the AI service registers, how ready each is, and what each
//! lists. The person's own choices about agents — which are turned off,
//! their order, the starred models — are the window's already, handed to
//! the tab ([`AgentSettings`]); nothing here reads them again.
//!
//! **An ordinary client, with one scope.** Every catalog read is
//! `ai.manage_providers`, which the chat's own reader holds ordinarily; the
//! Bot link's first-party scopes (`ai.chat`, `bots.manage`) would be
//! refused, and widening a first-party link is the wrong direction.
//!
//! **The window thread only `try_send`s and `try_recv`s** on bounded
//! channels; a service that is slow or gone costs it nothing. Every reply
//! carries the sequence number of its request and only the latest is
//! taken. **A failed read keeps what was shown**: an agent list or a listing
//! that does not come leaves the last one in place. **Never under test**:
//! [`CatalogLink::start`] answers `None` in this crate's suite, so a test
//! drives [`CatalogLink::test_pair`] instead.
//!
//! **Asked when shown, never on a timer.** A tab asks for the agents and
//! their readiness the first time it shows its settings or its create form,
//! and again when it is shown after being hidden ([`Catalog::hidden`]); a
//! listing when the control asks for one (`Outcome::Catalog`) or ↻.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

use koru_ai_core::AiProviderId;
use koru_ai_ui::agent_catalog::config::picker::{Agent, FavoriteModel, Listing, Readiness};
use koru_ai_ui::agent_catalog::{self, CatalogAgent, CatalogListing, ListingFailure};

/// Commands waiting for the thread: the agents, and an agent or two's
/// readiness and listing.
const COMMAND_CAPACITY: usize = 16;
/// Answers waiting for the window's next pass.
const ANSWER_CAPACITY: usize = 16;

pub(crate) enum Command {
    /// The agent runtimes the service registers.
    Agents { seq: u64 },
    /// How ready one agent is.
    Readiness { seq: u64, agent: AiProviderId },
    /// What one agent lists; `refresh` rebuilds it first (↻).
    Listing {
        seq: u64,
        agent: AiProviderId,
        refresh: bool,
    },
}

#[derive(Debug)]
pub(crate) enum Answer {
    Agents {
        seq: u64,
        result: Result<Vec<CatalogAgent>, Failure>,
    },
    Readiness {
        seq: u64,
        agent: AiProviderId,
        result: Result<Readiness, Failure>,
    },
    Listing {
        seq: u64,
        agent: AiProviderId,
        result: Result<CatalogListing, ListingFailure>,
    },
}

/// Why a read did not answer: nothing answered, or a refusal by its code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Unreachable,
    Refused(String),
}

fn failure(code: String) -> Failure {
    if code.is_empty() {
        Failure::Unreachable
    } else {
        Failure::Refused(code)
    }
}

pub(crate) struct CatalogLink {
    commands: SyncSender<Command>,
    answers: Receiver<Answer>,
}

impl CatalogLink {
    /// The link, its thread dialling `root`'s gateway when the window was
    /// given `--root`. `None` under test, and when the thread cannot start.
    pub(crate) fn start(root: Option<PathBuf>) -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (answer_tx, answers) = sync_channel(ANSWER_CAPACITY);
        std::thread::Builder::new()
            .name("koru-bots-catalog-client".into())
            .spawn(move || run(root, command_rx, answer_tx))
            .ok()?;
        Some(Self { commands, answers })
    }

    /// Queue `command`; `false` when the queue is full or the thread gone.
    pub(crate) fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// Every answer that has arrived, oldest first. Never blocks.
    pub(crate) fn drain(&self) -> Vec<Answer> {
        let mut out = Vec::new();
        loop {
            match self.answers.try_recv() {
                Ok(answer) => out.push(answer),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    /// A link with no thread behind it: the test holds the other ends.
    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, Receiver<Command>, SyncSender<Answer>) {
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (answer_tx, answers) = sync_channel(ANSWER_CAPACITY);
        (Self { commands, answers }, command_rx, answer_tx)
    }
}

/// The thread: connect when first asked, answer each command, drop the
/// client after a failure so the next command connects again.
fn run(root: Option<PathBuf>, commands: Receiver<Command>, answers: SyncSender<Answer>) {
    let mut client: Option<koru_api_client::Client> = None;
    while let Ok(command) = commands.recv() {
        if client.is_none() {
            client = connect(root.as_deref());
        }
        let answer = match (&client, command) {
            (Some(connected), Command::Agents { seq }) => Answer::Agents {
                seq,
                result: agent_catalog::agents(connected).map_err(failure),
            },
            (Some(connected), Command::Readiness { seq, agent }) => Answer::Readiness {
                seq,
                result: agent_catalog::readiness(connected, &agent).map_err(failure),
                agent,
            },
            (
                Some(connected),
                Command::Listing {
                    seq,
                    agent,
                    refresh,
                },
            ) => Answer::Listing {
                seq,
                result: agent_catalog::listing(connected, &agent, refresh),
                agent,
            },
            (None, Command::Agents { seq }) => Answer::Agents {
                seq,
                result: Err(Failure::Unreachable),
            },
            (None, Command::Readiness { seq, agent }) => Answer::Readiness {
                seq,
                agent,
                result: Err(Failure::Unreachable),
            },
            (None, Command::Listing { seq, agent, .. }) => Answer::Listing {
                seq,
                agent,
                result: Err(ListingFailure::Unreachable),
            },
        };
        let broken = match &answer {
            Answer::Agents { result, .. } => result.is_err(),
            Answer::Readiness { result, .. } => result.is_err(),
            Answer::Listing { result, .. } => {
                matches!(result, Err(ListingFailure::Unreachable))
            }
        };
        if broken {
            // A content-free line: the kind of read, never a name or a path.
            tracing::info!(target: "koru::bots", "A Bot catalog read was not answered");
            client = None;
        }
        if answers.send(answer).is_err() {
            return;
        }
    }
}

fn connect(root: Option<&std::path::Path>) -> Option<koru_api_client::Client> {
    let segment = koru_channel::segment();
    match root {
        Some(root) => {
            koru_api_client::Client::connect_at_with_scopes(segment, root, agent_catalog::SCOPES)
        }
        None => koru_api_client::Client::connect_with_scopes(segment, agent_catalog::SCOPES),
    }
    .ok()
}

/// **The person's own choices about agents, as the window holds them**:
/// the agents turned off on this machine, the order they are listed in, the
/// starred models and the agent a new chat starts with. The window hands
/// them to every Bot tab (`MainView::apply_hidden_agents`).
// Read by the settings card and the create form (S7, S8).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentSettings {
    pub hidden: Vec<String>,
    pub order: Vec<String>,
    pub favorites: Vec<FavoriteModel>,
    pub default_agent: Option<String>,
}

/// What is known of one agent's models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Known {
    Loading,
    Failed,
    Listed(CatalogListing),
}

/// **The catalog as a tab holds it**: what the service said, what is asked,
/// and the link it asks through. Pure but for the link, which is `None`
/// under test unless one is handed in.
#[derive(Default)]
pub(crate) struct Catalog {
    link: Option<CatalogLink>,
    started: bool,
    seq: u64,
    /// The agent runtimes, once the service has said; kept when a later
    /// read fails.
    agents: Option<Vec<CatalogAgent>>,
    agents_seq: Option<u64>,
    /// The agents must be read again: never read, or shown after hidden.
    agents_owed: bool,
    readiness: HashMap<AiProviderId, Readiness>,
    readiness_seq: HashMap<AiProviderId, u64>,
    readiness_owed: HashSet<AiProviderId>,
    listings: HashMap<AiProviderId, Known>,
    listing_seq: HashMap<AiProviderId, u64>,
    /// Listings to ask for, and whether each is a ↻.
    listing_owed: Vec<(AiProviderId, bool)>,
    /// ↻s answered since the last drain, for the control's *Checking…*.
    refreshed: Vec<AiProviderId>,
    /// Requests handed to the link, for a test to count.
    sent: usize,
}

impl Catalog {
    /// A catalog over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(link: CatalogLink) -> Self {
        Self {
            link: Some(link),
            started: true,
            ..Self::default()
        }
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    /// The agents, as the service last said; empty until it has.
    pub(crate) fn agents(&self) -> &[CatalogAgent] {
        self.agents.as_deref().unwrap_or_default()
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn readiness(&self, id: &AiProviderId) -> Option<Readiness> {
        self.readiness.get(id).copied()
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn listing(&self, id: &AiProviderId) -> Option<&CatalogListing> {
        match self.listings.get(id) {
            Some(Known::Listed(listing)) => Some(listing),
            _ => None,
        }
    }

    /// Requests handed to the link. Test only.
    #[cfg(test)]
    pub(crate) fn sent(&self) -> usize {
        self.sent
    }

    /// **Shown, for the first time or after being hidden**: the agents and
    /// their readiness are read on the next pass. A listing already had is
    /// kept; ↻ is how a person asks for a new one.
    pub(crate) fn shown(&mut self) {
        self.agents_owed = true;
    }

    /// **`agent`'s listing is wanted** (the control's `Outcome::Catalog`, or
    /// the draft's own agent): asked unless it is listed or on its way.
    pub(crate) fn want(&mut self, agent: &AiProviderId) {
        if matches!(self.listings.get(agent), Some(Known::Listed(_)))
            || self.listing_seq.contains_key(agent)
            || self.listing_owed.iter().any(|(owed, _)| owed == agent)
        {
            return;
        }
        self.listing_owed.push((agent.clone(), false));
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    /// **↻ for `agent`**: its list rebuilt, asked even while one is listed.
    pub(crate) fn refresh(&mut self, agent: &AiProviderId) {
        self.listing_owed.retain(|(owed, _)| owed != agent);
        self.listing_owed.push((agent.clone(), true));
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    /// The ↻s answered since the last call, for the control's state.
    pub(crate) fn take_refreshed(&mut self) -> Vec<AiProviderId> {
        std::mem::take(&mut self.refreshed)
    }

    /// **The pass a shown tab runs**: start the link the first time, send
    /// what is owed, fold what came back. `true` when what is shown moved.
    pub(crate) fn pass(&mut self, root: Option<PathBuf>) -> bool {
        if !self.started {
            self.started = true;
            self.link = CatalogLink::start(root);
            self.agents_owed = true;
        }
        let moved = self.fold();
        self.send_owed();
        moved
    }

    /// Whether an answer is owed: the tab keeps a short clock while one is,
    /// so it is taken when it comes.
    pub(crate) fn waiting(&self) -> bool {
        self.agents_seq.is_some() || !self.readiness_seq.is_empty() || !self.listing_seq.is_empty()
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn send_owed(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        if self.agents_owed && self.agents_seq.is_none() {
            let seq = self.next_seq();
            if link.send(Command::Agents { seq }) {
                self.agents_seq = Some(seq);
                self.agents_owed = false;
                self.sent += 1;
            }
        }
        for agent in std::mem::take(&mut self.readiness_owed) {
            let seq = self.next_seq();
            if link.send(Command::Readiness {
                seq,
                agent: agent.clone(),
            }) {
                self.readiness_seq.insert(agent, seq);
                self.sent += 1;
            } else {
                self.readiness_owed.insert(agent);
            }
        }
        let owed = std::mem::take(&mut self.listing_owed);
        for (agent, refresh) in owed {
            let seq = self.next_seq();
            if link.send(Command::Listing {
                seq,
                agent: agent.clone(),
                refresh,
            }) {
                self.listing_seq.insert(agent.clone(), seq);
                self.listings
                    .entry(agent)
                    .and_modify(|known| {
                        if *known == Known::Failed {
                            *known = Known::Loading;
                        }
                    })
                    .or_insert(Known::Loading);
                self.sent += 1;
            } else {
                self.listing_owed.push((agent, refresh));
            }
        }
        self.link = Some(link);
    }

    fn fold(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let mut moved = false;
        for answer in link.drain() {
            match answer {
                Answer::Agents { seq, result } if self.agents_seq == Some(seq) => {
                    self.agents_seq = None;
                    // A failed read keeps what was shown.
                    if let Ok(agents) = result {
                        for agent in &agents {
                            self.readiness_owed.insert(agent.id.clone());
                        }
                        if self.agents.as_ref() != Some(&agents) {
                            self.agents = Some(agents);
                            moved = true;
                        }
                    }
                }
                Answer::Readiness { seq, agent, result }
                    if self.readiness_seq.get(&agent) == Some(&seq) =>
                {
                    self.readiness_seq.remove(&agent);
                    if let Ok(readiness) = result
                        && self.readiness.insert(agent, readiness) != Some(readiness)
                    {
                        moved = true;
                    }
                }
                Answer::Listing { seq, agent, result }
                    if self.listing_seq.get(&agent) == Some(&seq) =>
                {
                    self.listing_seq.remove(&agent);
                    self.refreshed.push(agent.clone());
                    let known = match (result, self.listings.remove(&agent)) {
                        (Ok(listing), _) => Known::Listed(listing),
                        // Kept: a listing shown is not taken away by a read
                        // that did not come.
                        (Err(_), Some(Known::Listed(listing))) => Known::Listed(listing),
                        // Still being built: it says so, and is asked again
                        // when the control next wants it.
                        (Err(ListingFailure::StillListing), _) => Known::Loading,
                        (Err(_), _) => Known::Failed,
                    };
                    if known == Known::Loading {
                        self.listings.remove(&agent);
                    } else {
                        self.listings.insert(agent, known);
                    }
                    moved = true;
                }
                // An older request's answer: a newer one is on its way.
                _ => {}
            }
        }
        moved
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    /// **The agents as the control takes them**, in the person's order, each
    /// with its readiness, whether it is turned off and its listing —
    /// borrowed from what the catalog holds. Every registered agent runtime
    /// is offered; one turned off is off the rail unless it is the current
    /// one, as in the chat.
    pub(crate) fn control_agents<'a>(
        &'a self,
        settings: &AgentSettings,
    ) -> Vec<Agent<'a, AiProviderId>> {
        let mut agents: Vec<&CatalogAgent> = self.agents().iter().collect();
        agents.sort_by_key(|agent| {
            agent
                .key
                .as_deref()
                .and_then(|key| settings.order.iter().position(|known| known == key))
                .unwrap_or(usize::MAX)
        });
        agents
            .into_iter()
            .map(|agent| Agent {
                id: agent.id.clone(),
                key: agent.key.as_deref(),
                label: &agent.label,
                mark: agent.mark,
                readiness: self.readiness(&agent.id),
                hidden: agent
                    .key
                    .as_deref()
                    .is_some_and(|key| settings.hidden.iter().any(|off| off == key)),
                listing: match self.listings.get(&agent.id) {
                    Some(Known::Listed(listing)) => Listing::Listed(listing.records()),
                    Some(Known::Failed) => Listing::Failed,
                    Some(Known::Loading) | None => Listing::Loading,
                },
            })
            .collect()
    }

    // Read by the settings card and the create form (S7, S8).
    #[cfg_attr(not(test), allow(dead_code))]
    /// The agent a new Bot starts on: the person's default agent where it is
    /// registered and not turned off, else the first ready one in their
    /// order, else the first. Never a fallback the person did not see: the
    /// form shows it and its readiness.
    pub(crate) fn preferred(&self, settings: &AgentSettings) -> Option<AiProviderId> {
        let agents = self.control_agents(settings);
        let shown: Vec<&Agent<'_, AiProviderId>> =
            agents.iter().filter(|agent| !agent.hidden).collect();
        settings
            .default_agent
            .as_deref()
            .and_then(|key| shown.iter().find(|agent| agent.key == Some(key)))
            .or_else(|| {
                shown
                    .iter()
                    .find(|agent| agent.readiness == Some(Readiness::Ready))
            })
            .or_else(|| shown.first())
            .map(|agent| agent.id.clone())
    }
}

#[cfg(test)]
mod tests;
