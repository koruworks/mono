// SPDX-License-Identifier: MPL-2.0

//! The owner feed in the AI worker: pages of Bot client protocol 1 built
//! from the markers and subjects the repository reads.
//!
//! **What is here.** One start of the worker ([`OwnerFeed::start`]), the
//! decision a `subscribe` gets ([`OwnerFeed::subscribe`]), one page
//! ([`OwnerFeed::page`]), the reconcile that keeps a Bot whose workspace is
//! kept on this computer out of every copy ([`OwnerFeed::reconcile`]), the
//! check a linked desktop's command gets before it names a Bot
//! ([`OwnerFeed::command_target`]), and the writer's budget
//! ([`FeedBudget`]). Nothing here holds a stream, a peer or a timer.
//!
//! **Who is never sent.** A record is built only for a Bot of this person on
//! this computer whose home workspace the Store says may leave it. Every
//! other Bot, and each conversation and message of it, is sent as removed.
//! When the Store cannot be asked, the Bot is withheld for that page.
//!
//! **The Store is asked off the writer when the caller wants that.** A page
//! is built in two steps: [`OwnerFeed::page_homes`] names the workspaces to
//! ask about and [`HomeCopies::ask`] asks, which may run anywhere; then
//! [`OwnerFeed::page`] builds the page on the writer with the answers. The
//! `_with` forms do the three in one call.
//!
//! No message, title or path is logged or printed from here: the records'
//! `Debug` prints a kind and a format, and this module logs nothing.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use koru_ai_core::WorkspaceId;
use koru_ai_core::bots::{BotAuthority, BotError, BotId, BotOwner, HomeCopy};
use koru_ai_sqlite::{FeedBody, FeedBotHome, FeedItem, FeedKind, FeedMessage, Repository};
use koru_ai_wire::{MessageSources, MessageUsage};
use koru_bot_client_wire::{
    BotRecord, Changes, ChatActivity, ChatRecord, Entry, Gap, GapReason, Limits, LiveCounts,
    MessageRecord, PageBuilder, REPLICA_FORMAT, Record, Subscribe, TeamPauseRecord, limits,
};

/// How often the worker reconciles while a linked desktop is subscribed.
pub(crate) const RECONCILE_EVERY: Duration = Duration::from_secs(60);

/// The home workspaces the Store was asked about, and those it said may be
/// copied. A workspace that was not asked about, or whose answer was a
/// failure, is not in `copied`: the Bot is withheld.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HomeCopies {
    asked: BTreeSet<WorkspaceId>,
    copied: BTreeSet<WorkspaceId>,
}

impl HomeCopies {
    /// Ask once for each distinct workspace. May run off the writer: it
    /// reads nothing of `ai.db`.
    pub(crate) fn ask(
        authority: &dyn BotAuthority,
        homes: impl IntoIterator<Item = WorkspaceId>,
    ) -> Self {
        let mut copies = Self::default();
        for home in homes {
            if copies.asked.insert(home)
                && matches!(authority.home_copy(home), Ok(HomeCopy::Copied))
            {
                copies.copied.insert(home);
            }
        }
        copies
    }

    pub(crate) fn is_copied(&self, home: WorkspaceId) -> bool {
        self.copied.contains(&home)
    }

    /// Add the answers of another ask.
    pub(crate) fn merge(&mut self, other: Self) {
        self.asked.extend(other.asked);
        self.copied.extend(other.copied);
    }

    fn knows(&self, home: WorkspaceId) -> bool {
        self.asked.contains(&home)
    }
}

/// What a `subscribe` is answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Subscription {
    /// The cursor names history this owner still has: pages follow from it.
    Continue { cursor: u64 },
    /// One `gap`, then pages from cursor 0 with `reset: true`.
    Reset(Gap),
}

/// Why a `subscribe` or a page was not served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeedRefusal {
    /// The message names another computer: `refused { wrong_owner }`.
    WrongOwner,
    /// Some of the three members are null and some are not.
    Malformed,
    /// The repository or the Store could not answer: `bots_unavailable`.
    Unavailable,
}

impl From<BotError> for FeedRefusal {
    fn from(_: BotError) -> Self {
        Self::Unavailable
    }
}

/// What one page build answers.
#[derive(Debug)]
pub(crate) enum PageOutcome {
    Page(Changes),
    /// The Store was not asked about these workspaces, which the next
    /// markers need. Ask, and build again.
    Ask(Vec<WorkspaceId>),
    /// A marker and the Store disagree about a Bot being withheld.
    /// Reconcile, and build again; no page was built.
    Reconcile,
}

/// What a reconcile did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Bots whose markers were rewritten.
    pub(crate) changed: u32,
    /// This person's Bots on this computer that are withheld.
    pub(crate) withheld_bots: u32,
}

/// The owner feed for one run of the AI worker.
///
/// It exists only once the start is recorded and the first reconcile has
/// finished ([`Self::start`]), so no page can be built before either.
#[derive(Debug)]
pub(crate) struct OwnerFeed {
    owner: BotOwner,
    feed_id: String,
    boot_id: String,
    withheld_bots: u32,
}

impl OwnerFeed {
    /// A start's identity: 32 hexadecimal characters, drawn once per run.
    pub(crate) fn new_boot_id() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    /// Record this start of the worker and reconcile every Bot's marker with
    /// the Store. The migration could not ask the Store, so it marked no Bot
    /// withheld; this pass does, and it finishes before the feed exists.
    // The owner server takes the split form, so that the Store is asked off
    // the writer; this one is what the feed's own tests drive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn start(
        repository: &mut Repository,
        authority: &dyn BotAuthority,
        boot_id: String,
    ) -> Result<Self, FeedRefusal> {
        let owner = authority.owner()?;
        let feed_id = repository.feed_id()?;
        repository.feed_start(&boot_id)?;
        let mut feed = Self {
            owner,
            feed_id,
            boot_id,
            withheld_bots: 0,
        };
        feed.reconcile_with(repository, authority)?;
        Ok(feed)
    }

    /// The first half of a start whose Store reads run off the writer: the
    /// feed of a start the worker already recorded (`boot_id` is the newest
    /// row of `bot_client_feed_boots`), and every Bot that has a marker.
    /// The caller asks the Store about the homes of the local ones and
    /// hands the answers to [`PendingFeed::reconciled`]. `owner` is what the
    /// authority answered, asked off the writer too.
    ///
    /// What comes back is not a feed: no page can be built from it until
    /// that first reconcile has run.
    pub(crate) fn resume(
        repository: &Repository,
        owner: BotOwner,
        boot_id: String,
    ) -> Result<(PendingFeed, Vec<FeedBotHome>), FeedRefusal> {
        if repository.feed_current_start()?.as_deref() != Some(boot_id.as_str()) {
            return Err(FeedRefusal::Unavailable);
        }
        let feed = Self {
            owner,
            feed_id: repository.feed_id()?,
            boot_id,
            withheld_bots: 0,
        };
        let homes = feed.reconcile_homes(repository)?;
        Ok((PendingFeed(feed), homes))
    }

    pub(crate) fn owner(&self) -> BotOwner {
        self.owner
    }
    pub(crate) fn feed_id(&self) -> &str {
        &self.feed_id
    }
    pub(crate) fn boot_id(&self) -> &str {
        &self.boot_id
    }
    /// The feed's newest `seq`, for the hello.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn head(&self, repository: &Repository) -> Result<u64, FeedRefusal> {
        Ok(repository.feed_head()?)
    }
    /// This person's Bots on this computer that the last reconcile withheld.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn withheld_bots(&self) -> u32 {
        self.withheld_bots
    }

    /// Every Bot that has a marker, for a reconcile whose Store reads run
    /// off the writer: ask about the homes of the local ones, then
    /// [`Self::reconcile`].
    pub(crate) fn reconcile_homes(
        &self,
        repository: &Repository,
    ) -> Result<Vec<FeedBotHome>, FeedRefusal> {
        Ok(repository.feed_bot_homes(self.owner)?)
    }

    /// Compare each Bot's marker with the Store's answer and rewrite the
    /// markers of the Bots where they differ. A Bot that is not this
    /// person's on this computer is marked withheld and is not counted
    /// among the withheld Bots a linked desktop is told of.
    pub(crate) fn reconcile(
        &mut self,
        repository: &mut Repository,
        homes: &[FeedBotHome],
        copies: &HomeCopies,
    ) -> Result<Reconciled, FeedRefusal> {
        let mut done = Reconciled::default();
        for home in homes {
            let withhold = !(home.local && copies.is_copied(home.home));
            if home.local && withhold {
                done.withheld_bots += 1;
            }
            if withhold != home.withheld && repository.feed_set_withheld(home.bot, withhold)? {
                done.changed += 1;
            }
        }
        self.withheld_bots = done.withheld_bots;
        Ok(done)
    }

    /// [`Self::reconcile`] with the Store asked on this thread.
    // The owner server takes the split form, so that the Store is asked off
    // the writer; this one is what the feed's own tests drive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reconcile_with(
        &mut self,
        repository: &mut Repository,
        authority: &dyn BotAuthority,
    ) -> Result<Reconciled, FeedRefusal> {
        let homes = self.reconcile_homes(repository)?;
        let copies = HomeCopies::ask(
            authority,
            homes.iter().filter(|home| home.local).map(|home| home.home),
        );
        self.reconcile(repository, &homes, &copies)
    }

    /// Decide a `subscribe` (contract 4.6). `again` is true when this
    /// connection already subscribed once, which is how a fresh subscribe
    /// that asks for a reset is told from a first one.
    pub(crate) fn subscribe(
        &self,
        repository: &Repository,
        subscribe: &Subscribe,
        again: bool,
    ) -> Result<Subscription, FeedRefusal> {
        if subscribe.owner_device != self.owner.device {
            return Err(FeedRefusal::WrongOwner);
        }
        let head = repository.feed_head()?;
        let reset = |reason| {
            Subscription::Reset(Gap {
                owner_device: self.owner.device,
                reason,
                head,
            })
        };
        if subscribe.is_fresh() {
            return Ok(reset(if again {
                GapReason::ClientRequested
            } else {
                GapReason::FirstSubscribe
            }));
        }
        let (Some(feed_id), Some(boot_id), Some(cursor)) =
            (&subscribe.feed_id, &subscribe.boot_id, subscribe.cursor)
        else {
            return Err(FeedRefusal::Malformed);
        };
        if *feed_id != self.feed_id {
            return Ok(reset(GapReason::FeedReplaced));
        }
        if !repository.feed_cursor_served(feed_id, boot_id, cursor)? {
            return Ok(reset(GapReason::FeedRewound));
        }
        Ok(Subscription::Continue { cursor })
    }

    /// The home workspaces the next page's markers need the Store asked
    /// about.
    pub(crate) fn page_homes(
        &self,
        repository: &Repository,
        cursor: u64,
    ) -> Result<Vec<WorkspaceId>, FeedRefusal> {
        Ok(repository.feed_page_homes(cursor, limits::MAX_PAGE_ENTRIES as u16)?)
    }

    /// One page above `cursor`: at most 64 entries and 524,288 bytes of
    /// them, and always at least one entry when there is one. `reset` is
    /// true on every page of a reset. One short read on the writer.
    pub(crate) fn page(
        &self,
        repository: &Repository,
        cursor: u64,
        reset: bool,
        copies: &HomeCopies,
    ) -> Result<PageOutcome, FeedRefusal> {
        let unasked: Vec<WorkspaceId> = self
            .page_homes(repository, cursor)?
            .into_iter()
            .filter(|home| !copies.knows(*home))
            .collect();
        if !unasked.is_empty() {
            return Ok(PageOutcome::Ask(unasked));
        }
        let scan = repository.feed_scan(
            self.owner,
            cursor,
            limits::MAX_PAGE_ENTRIES as u16,
            // Read a little past what a page may hold, so a page of small
            // records is filled; a page's own byte limit is applied below.
            2 * limits::MAX_PAGE_BYTES as usize,
            &copies.copied,
        )?;
        if scan.stale_withheld {
            return Ok(PageOutcome::Reconcile);
        }
        let read = scan.items.len();
        let mut builder = PageBuilder::new(Limits::this_build());
        let mut to_cursor = cursor;
        let mut taken = 0;
        for item in scan.items {
            let entry = entry_of(item);
            let seq = entry.seq;
            if builder.push(entry).is_err() {
                break;
            }
            to_cursor = seq;
            taken += 1;
        }
        let caught_up = scan.exhausted && taken == read;
        let counts = if caught_up {
            let counts = repository.feed_counts()?;
            Some(LiveCounts {
                bots: counts.bots,
                chats: counts.chats,
                messages: counts.messages,
            })
        } else {
            None
        };
        Ok(PageOutcome::Page(Changes {
            owner_device: self.owner.device,
            feed_id: self.feed_id.clone(),
            boot_id: self.boot_id.clone(),
            from_cursor: cursor,
            to_cursor,
            reset,
            caught_up,
            head: scan.head.max(to_cursor),
            live: counts,
            withheld_bots: caught_up.then_some(self.withheld_bots),
            entries: builder.finish(),
        }))
    }

    /// [`Self::page`] with the Store asked, and the markers reconciled when
    /// the page finds a difference, on this thread.
    // The owner server takes the split form, so that the Store is asked off
    // the writer; this one is what the feed's own tests drive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn page_with(
        &mut self,
        repository: &mut Repository,
        authority: &dyn BotAuthority,
        cursor: u64,
        reset: bool,
    ) -> Result<Changes, FeedRefusal> {
        let mut copies = HomeCopies::default();
        // Each round asks about workspaces not yet asked about, or
        // reconciles once; neither can repeat without end.
        for _ in 0..4 {
            match self.page(repository, cursor, reset, &copies)? {
                PageOutcome::Page(page) => return Ok(page),
                PageOutcome::Ask(homes) => {
                    copies.merge(HomeCopies::ask(authority, homes));
                }
                PageOutcome::Reconcile => {
                    self.reconcile_with(repository, authority)?;
                }
            }
        }
        Err(FeedRefusal::Unavailable)
    }

    /// Whether a linked desktop's command may name `bot`: it is this
    /// person's on this computer and is not withheld. Anything else is
    /// `NotFound`, the answer for a Bot that does not exist, so a withheld
    /// Bot cannot be told from an absent one.
    // The owner server takes the split form, so that the Store is asked off
    // the writer; this one is what the feed's own tests drive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn command_target(
        &self,
        repository: &Repository,
        authority: &dyn BotAuthority,
        bot: BotId,
    ) -> Result<(), BotError> {
        let home = self.command_home(repository, bot)?;
        command_home_copied(authority, home)
    }

    /// The first half of [`Self::command_target`], on the writer: the home
    /// workspace of a Bot that is this person's on this computer. The Store
    /// is then asked about that home off the writer
    /// ([`command_home_copied`]).
    pub(crate) fn command_home(
        &self,
        repository: &Repository,
        bot: BotId,
    ) -> Result<WorkspaceId, BotError> {
        let Some((bot, _)) = repository.bot_any_lifecycle(bot)? else {
            return Err(BotError::NotFound);
        };
        if bot.user_scope != self.owner.user_scope || bot.owner_device != self.owner.device {
            return Err(BotError::NotFound);
        }
        Ok(bot.home_workspace)
    }
}

/// The second half of [`OwnerFeed::command_target`]: whether Bots whose home
/// is `home` are copied. A home kept on this computer, and a Store that does
/// not answer, are both `NotFound`. Reads nothing of `ai.db`.
pub(crate) fn command_home_copied(
    authority: &dyn BotAuthority,
    home: WorkspaceId,
) -> Result<(), BotError> {
    match authority.home_copy(home) {
        Ok(HomeCopy::Copied) => Ok(()),
        Ok(HomeCopy::LocalOnly) | Err(_) => Err(BotError::NotFound),
    }
}

/// A feed whose start is recorded and whose first reconcile has not run
/// ([`OwnerFeed::resume`]). It serves nothing.
#[derive(Debug)]
pub(crate) struct PendingFeed(OwnerFeed);

impl PendingFeed {
    /// Run the first reconcile with the Store's answers and hand the feed
    /// over. A marker and the Store may still disagree for a Bot created in
    /// between; a page build finds that and asks for another reconcile.
    pub(crate) fn reconciled(
        self,
        repository: &mut Repository,
        homes: &[FeedBotHome],
        copies: &HomeCopies,
    ) -> Result<OwnerFeed, FeedRefusal> {
        let mut feed = self.0;
        feed.reconcile(repository, homes, copies)?;
        Ok(feed)
    }
}

/// One marker and its subject as a feed entry.
fn entry_of(item: FeedItem) -> Entry {
    let FeedItem { marker, body } = item;
    // The same members a present entry of the kind carries: a Bot's entry
    // names the Bot by its id alone, a conversation's names its Bot, a
    // message's names its Bot and its conversation.
    let (bot_id, chat_id) = match marker.kind {
        FeedKind::Bot | FeedKind::TeamPause => (None, None),
        FeedKind::Chat => (marker.bot_id.map(|bot| bot.to_string()), None),
        FeedKind::Message => (
            marker.bot_id.map(|bot| bot.to_string()),
            marker.chat_id.map(|chat| chat.to_string()),
        ),
    };
    let record = match body {
        FeedBody::Removed => {
            return Entry::removed(
                marker.seq,
                marker.kind.token(),
                marker.subject_id,
                bot_id,
                chat_id,
            );
        }
        FeedBody::Bot(bot) => Record::from(BotRecord::new(*bot)),
        FeedBody::Chat(chat) => {
            let (run_id, run_workspace_id) = chat.run.unzip();
            Record::from(ChatRecord {
                format: REPLICA_FORMAT,
                chat: chat.chat,
                bot_id: chat.bot_id,
                purpose: chat.purpose,
                trashed: chat.trashed,
                activity: ChatActivity {
                    running: chat.running,
                    needs_you: chat.needs_you,
                },
                run_id,
                run_workspace_id,
            })
        }
        FeedBody::Message(message) => Record::from(message_record(*message)),
        FeedBody::Team(team) => Record::from(TeamPauseRecord::new(*team)),
    };
    let mut entry = Entry::present(marker.seq, marker.subject_id, record);
    // A message's record does not say which Bot it belongs to; its entry
    // does.
    if marker.kind == FeedKind::Message {
        entry.bot_id = bot_id;
    }
    entry
}

/// A tool call as a linked desktop is sent it, in a copied message, in a
/// live event and in a command's answer alike: its name, its state and the
/// question a permission request asks, and never its arguments, its results,
/// the path it shows or its live wording.
pub(crate) fn public_tool_activity(activity: &mut koru_ai_core::ToolActivity) {
    activity.agent_input = None;
    activity.agent_output = None;
    activity.display_target = None;
    activity.live = None;
}

/// A message's record. Its tool activity carries no arguments and no
/// results: the repository did not read them, and they are cleared here
/// again so that no other source of a [`FeedMessage`] can put them in.
fn message_record(message: FeedMessage) -> MessageRecord {
    let FeedMessage {
        message,
        blocks,
        mut tool_activities,
        notices,
        feedback,
        attempt,
        run,
        sources,
        attachment_count,
    } = message;
    for activity in &mut tool_activities {
        public_tool_activity(activity);
    }
    let mut usage = attempt
        .and_then(crate::worker::attempt_usage)
        .and_then(|(_, entry)| entry);
    if let Some(run) = run {
        usage
            .get_or_insert_with(|| MessageUsage {
                message_id: message.id,
                usage: koru_ai_core::AiUsage::default(),
                context: None,
                stop_code: None,
                run_id: None,
            })
            .run_id = Some(run);
    }
    let sources = (!sources.is_empty()).then(|| MessageSources {
        message_id: message.id,
        sources,
    });
    MessageRecord {
        format: REPLICA_FORMAT,
        message,
        blocks,
        tool_activities,
        notices,
        feedback,
        usage,
        sources,
        attachment_count,
        body_omitted: false,
    }
    .fitted()
}

/// When the writer may build a feed page (contract 5.1): only when it has no
/// queued command, or when 250 ms have passed since its last page; and at
/// most one page per 50 ms during a reset. A reset of a long history
/// therefore yields to chat writes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FeedBudget {
    last_page: Option<Instant>,
}

impl FeedBudget {
    /// With a command queued, a page waits this long after the last one.
    pub(crate) const STARVED_AFTER: Duration = Duration::from_millis(250);
    /// During a reset, pages are at least this far apart.
    pub(crate) const RESET_SPACING: Duration = Duration::from_millis(50);

    /// Whether a page may be built at `now`, with `queued_commands` waiting
    /// for the writer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn may_build(&self, now: Instant, queued_commands: usize, reset: bool) -> bool {
        self.wait(now, queued_commands, reset).is_none()
    }

    /// How long until a page may be built if nothing else changes: `None`
    /// when it may be built now.
    pub(crate) fn wait(
        &self,
        now: Instant,
        queued_commands: usize,
        reset: bool,
    ) -> Option<Duration> {
        let last = self.last_page?;
        let since = now.saturating_duration_since(last);
        let spacing = if queued_commands > 0 {
            Self::STARVED_AFTER
        } else if reset {
            Self::RESET_SPACING
        } else {
            Duration::ZERO
        };
        spacing.checked_sub(since).filter(|left| !left.is_zero())
    }

    /// A page was built at `now`.
    pub(crate) fn built(&mut self, now: Instant) {
        self.last_page = Some(now);
    }
}

#[cfg(test)]
mod tests;
