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
//! **An ordinary client, with one scope** ([`SCOPES`]). Every catalog read
//! is `ai.manage_providers`, which the chat's own reader holds ordinarily;
//! the Bot link's first-party scopes (`ai.chat`, `bots.manage`) would be
//! refused, and widening a first-party link is the wrong direction. No
//! narrower scope serves these reads: the gateway's routes for provider
//! settings, validation and an agent's models all require
//! `ai.manage_providers` (`koru-api-gateway-core/src/ai.rs`); `ai.chat`
//! covers only an agent's commands.
//!
//! **The calls are made here, never in the shared crate.** `koru-ai-ui`'s
//! catalog module only converts what this thread asked for
//! (`agents_from`, `readiness_from`, `listing_from`), so it stays a
//! component another project can take without this app's service.
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

/// The scopes a catalog client asks for — and, because naming scopes
/// filters rather than adds, the only ones it holds: `provider_settings`,
/// `validate_provider`, `discover_agent_models` and `refresh_agent_models`
/// are each `ai.manage_providers`.
const SCOPES: &[&str] = &["ai.manage_providers"];

/// **How long an agent may go on answering *checking*** — or not answer —
/// **before the catalog stops asking and says it could not tell.** The
/// readiness probe answers *checking* while it re-probes, and a machine
/// where nothing can run (no runtime installed, no network) can answer that
/// for good: the first window run showed *Checking…* for more than two
/// minutes, with no sentence and no end.
pub(crate) const CHECKING_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
/// The bounded clock a readiness still being checked is asked again on.
const READINESS_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const READINESS_MAX: std::time::Duration = std::time::Duration::from_secs(8);

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

/// Why a read did not answer: the Bot link's one classification
/// ([`Failure::of`]), so a lost connection reads the same here as there.
pub(crate) use super::link::Failure;

fn failure(code: String, retryable: bool) -> Failure {
    Failure::of(super::link::Said { code, retryable })
}

/// **A listing's failure as the control reads it**: the shared crate's
/// reading of its code, except that an answer the link classifies as
/// unanswered is *nothing answered* — the client is dropped and dialled
/// again — whatever its code.
fn listing_failed(code: String, retryable: bool, shared: ListingFailure) -> ListingFailure {
    match (failure(code, retryable), shared) {
        (_, ListingFailure::StillListing) => ListingFailure::StillListing,
        (Failure::Unanswered, _) => ListingFailure::Unreachable,
        (Failure::Refused(_), shared) => shared,
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
                result: connected
                    .ai()
                    .provider_settings()
                    .map(|settings| agent_catalog::agents_from(&settings))
                    .map_err(|refused| failure(refused.code, refused.retryable)),
            },
            (Some(connected), Command::Readiness { seq, agent }) => Answer::Readiness {
                seq,
                result: connected
                    .ai()
                    .validate_provider(&agent)
                    .map(|validation| agent_catalog::readiness_from(&validation))
                    .map_err(|refused| failure(refused.code, refused.retryable)),
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
                result: listing(connected, &agent, refresh),
                agent,
            },
            (None, Command::Agents { seq }) => Answer::Agents {
                seq,
                result: Err(Failure::Unanswered),
            },
            (None, Command::Readiness { seq, agent }) => Answer::Readiness {
                seq,
                agent,
                result: Err(Failure::Unanswered),
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

/// **What `agent` lists**, asked again while its host is still building it
/// (`agent_models_listing`), as the chat's reader asks; `refresh` rebuilds
/// it first (↻), and the asks after the first only wait for that rebuild.
fn listing(
    client: &koru_api_client::Client,
    agent: &AiProviderId,
    refresh: bool,
) -> Result<CatalogListing, ListingFailure> {
    let mut asks = 0;
    loop {
        asks += 1;
        let answer = if refresh && asks == 1 {
            client.ai().refresh_agent_models(agent)
        } else {
            client.ai().discover_agent_models(agent)
        };
        match answer {
            Ok(response) => return Ok(agent_catalog::listing_from(&response)),
            Err(refused)
                if agent_catalog::is_still_listing(&refused)
                    && asks < agent_catalog::LISTING_ASKS => {}
            Err(refused) => {
                let shared = agent_catalog::listing_failure(&refused);
                return Err(listing_failed(refused.code, refused.retryable, shared));
            }
        }
    }
}

fn connect(root: Option<&std::path::Path>) -> Option<koru_api_client::Client> {
    let segment = koru_channel::segment();
    match root {
        Some(root) => koru_api_client::Client::connect_at_with_scopes(segment, root, SCOPES),
        None => koru_api_client::Client::connect_with_scopes(segment, SCOPES),
    }
    .ok()
}

/// **The person's own choices about agents, as the window holds them**:
/// the agents turned off on this machine, the order they are listed in, the
/// starred models and the agent a new chat starts with. The window hands
/// them to every Bot tab (`MainView::apply_hidden_agents`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentSettings {
    pub hidden: Vec<String>,
    pub order: Vec<String>,
    pub favorites: Vec<FavoriteModel>,
    pub default_agent: Option<String>,
}

/// **Where one agent's readiness stands**, for the sentence a Bot's card
/// says about it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    Ready,
    /// Answered *checking*, or not answered yet — and still being asked.
    Checking,
    /// Settled: installed or not, signed in or not, as the service said.
    NotReady(Readiness),
    /// Still checking, or unanswered, past [`CHECKING_LIMIT`].
    Unknown,
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
    /// **The agents' read did not answer**: since when, and when it is
    /// asked again — on the readiness clock, for at most [`CHECKING_LIMIT`]
    /// (a tab restored at startup reads before Koru AI is up; its form then
    /// offered no agent until it was hidden and shown again).
    agents_failing: Option<std::time::Instant>,
    agents_next: Option<std::time::Instant>,
    agents_backoff: std::time::Duration,
    readiness: HashMap<AiProviderId, Readiness>,
    readiness_seq: HashMap<AiProviderId, u64>,
    readiness_owed: HashSet<AiProviderId>,
    /// **Agents whose readiness is still being checked** — answered
    /// *checking*, or not answered — and since when: asked again on the
    /// bounded clock until [`CHECKING_LIMIT`].
    checking: HashMap<AiProviderId, std::time::Instant>,
    /// Agents that went on checking past the limit: said as *could not
    /// tell*, never as checking forever.
    unsettled: HashSet<AiProviderId>,
    readiness_next: Option<std::time::Instant>,
    readiness_backoff: std::time::Duration,
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

    /// The agents, as the service last said; empty until it has.
    pub(crate) fn agents(&self) -> &[CatalogAgent] {
        self.agents.as_deref().unwrap_or_default()
    }

    /// How ready `id` is, as the control is told: what the service said,
    /// and **`CheckFailed` once it went on checking past the limit** — the
    /// control's own word for a check that did not settle — so the rail and
    /// the list end in a word, never *Checking…* for good.
    pub(crate) fn readiness(&self, id: &AiProviderId) -> Option<Readiness> {
        if self.unsettled.contains(id) {
            return Some(Readiness::CheckFailed);
        }
        self.readiness.get(id).copied()
    }

    /// **Where `id`'s readiness stands, for the sentence on the card**.
    pub(crate) fn standing(&self, id: &AiProviderId) -> Standing {
        if self.unsettled.contains(id) {
            return Standing::Unknown;
        }
        match self.readiness.get(id) {
            Some(Readiness::Ready) => Standing::Ready,
            Some(Readiness::Checking) | None => Standing::Checking,
            Some(other) => Standing::NotReady(*other),
        }
    }

    /// **Every agent offered is known not to be able to run**: listed, and
    /// each one shown is settled not ready (or could not be told). The
    /// create form then refuses in words, before anything is sent
    /// (*No agent is set up on this computer yet…*). `false` while any is
    /// ready or still being checked, and before the agents are listed.
    pub(crate) fn none_can_run(&self, settings: &AgentSettings) -> bool {
        let agents = self.control_agents(settings);
        let shown: Vec<&Agent<'_, AiProviderId>> =
            agents.iter().filter(|agent| !agent.hidden).collect();
        !shown.is_empty()
            && shown.iter().all(|agent| {
                matches!(
                    self.standing(&agent.id),
                    Standing::NotReady(_) | Standing::Unknown
                )
            })
    }

    /// When the readiness still being checked, or the agents' read that did
    /// not answer, is next asked, while one is.
    pub(crate) fn deadline(&self) -> Option<std::time::Instant> {
        match (self.readiness_next, self.agents_next) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub(crate) fn listing(&self, id: &AiProviderId) -> Option<&CatalogListing> {
        match self.listings.get(id) {
            Some(Known::Listed(listing)) => Some(listing),
            _ => None,
        }
    }

    /// How ready `agent` is, as a test arms it.
    #[cfg(test)]
    pub(crate) fn force_readiness_for_test(&mut self, agent: &AiProviderId, readiness: Readiness) {
        self.readiness.insert(agent.clone(), readiness);
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
        // Asked afresh: an agent that could not be told is checked again,
        // with a new limit, rather than said as unknown for good.
        self.checking.clear();
        self.unsettled.clear();
        self.readiness_next = None;
        self.readiness_backoff = READINESS_FIRST;
        self.agents_failing = None;
        self.agents_next = None;
        self.agents_backoff = READINESS_FIRST;
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

    /// **↻ for `agent`**: its list rebuilt, asked even while one is listed.
    pub(crate) fn refresh(&mut self, agent: &AiProviderId) {
        self.listing_owed.retain(|(owed, _)| owed != agent);
        self.listing_owed.push((agent.clone(), true));
    }

    /// The ↻s answered since the last call, for the control's state.
    pub(crate) fn take_refreshed(&mut self) -> Vec<AiProviderId> {
        std::mem::take(&mut self.refreshed)
    }

    /// **The pass a shown tab runs**: start the link the first time, send
    /// what is owed, fold what came back. `true` when what is shown moved.
    pub(crate) fn pass(&mut self, root: Option<PathBuf>) -> bool {
        self.pass_at(root, std::time::Instant::now())
    }

    /// [`Self::pass`] at `now`: what a test drives to stand in for the time
    /// passing.
    pub(crate) fn pass_at(&mut self, root: Option<PathBuf>, now: std::time::Instant) -> bool {
        if !self.started {
            self.started = true;
            self.link = CatalogLink::start(root);
            self.agents_owed = true;
        }
        let moved = self.fold(now);
        // The agents' read that did not answer, due again.
        if self.agents_next.is_some_and(|at| at <= now) {
            self.agents_next = None;
            self.agents_owed = true;
        }
        // A readiness still being checked, due again.
        if self.readiness_next.is_some_and(|at| at <= now) {
            self.readiness_next = None;
            let due: Vec<AiProviderId> = self
                .checking
                .keys()
                .filter(|agent| !self.readiness_seq.contains_key(*agent))
                .cloned()
                .collect();
            self.readiness_owed.extend(due);
        }
        self.send_owed();
        moved
    }

    /// `agent`'s readiness did not settle at `now` — *checking*, or no
    /// answer: asked again on the clock, or said as unknown past the limit.
    /// `true` when that changed what is shown.
    fn still_checking(&mut self, agent: AiProviderId, now: std::time::Instant) -> bool {
        let since = *self.checking.entry(agent.clone()).or_insert(now);
        if now.saturating_duration_since(since) >= CHECKING_LIMIT {
            self.checking.remove(&agent);
            return self.unsettled.insert(agent);
        }
        if self.readiness_next.is_none() {
            let wait = self.readiness_backoff.max(READINESS_FIRST);
            self.readiness_next = Some(now.checked_add(wait).unwrap_or(now));
            self.readiness_backoff = (wait * 2).min(READINESS_MAX);
        }
        false
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

    fn fold(&mut self, now: std::time::Instant) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let mut moved = false;
        for answer in link.drain() {
            match answer {
                Answer::Agents { seq, result } if self.agents_seq == Some(seq) => {
                    self.agents_seq = None;
                    // A failed read keeps what was shown, and is asked again
                    // on the clock while that may help, within the limit.
                    if let Err(failure) = &result {
                        let since = *self.agents_failing.get_or_insert(now);
                        let again = failure.again() || failure.unvouched();
                        if again && now.saturating_duration_since(since) < CHECKING_LIMIT {
                            let wait = self.agents_backoff.max(READINESS_FIRST);
                            self.agents_next = Some(now.checked_add(wait).unwrap_or(now));
                            self.agents_backoff = (wait * 2).min(READINESS_MAX);
                        }
                    }
                    if let Ok(agents) = result {
                        self.agents_failing = None;
                        self.agents_next = None;
                        self.agents_backoff = READINESS_FIRST;
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
                    match result {
                        // Settled, either way: no more asking, and an agent
                        // once unknown is what the service says now.
                        Ok(readiness) if readiness != Readiness::Checking => {
                            self.checking.remove(&agent);
                            moved |= self.unsettled.remove(&agent);
                            moved |= self.readiness.insert(agent, readiness) != Some(readiness);
                        }
                        // Still being checked, or not answered (a failed read
                        // keeps what was shown): asked again, within limits.
                        Ok(readiness) => {
                            moved |=
                                self.readiness.insert(agent.clone(), readiness) != Some(readiness);
                            moved |= self.still_checking(agent, now);
                        }
                        // A failed read keeps a readiness that settled.
                        Err(_)
                            if self
                                .readiness
                                .get(&agent)
                                .is_some_and(|known| *known != Readiness::Checking) => {}
                        Err(_) => moved |= self.still_checking(agent, now),
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

    /// The agent a new Bot starts on: the person's default agent where it is
    /// registered and not turned off, else the first one in their order
    /// already known to be ready, else the first. The form seeds its draft
    /// once, as soon as the agents are listed — usually before any
    /// readiness has answered, so in practice that is the default agent or
    /// the first — and never swaps the agent afterwards: the person sees it
    /// and its readiness (S8, `BotView::seed_agent`).
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
