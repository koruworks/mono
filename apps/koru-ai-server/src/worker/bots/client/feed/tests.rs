// SPDX-License-Identifier: MPL-2.0
//! The owner feed in the worker, over an in-memory `ai.db` and a fake Bot
//! authority. No stream, peer, Store, delegate or provider.
//!
//! Cohorts `m8_feed_pages`, `m8_feed_starts`, `m8_feed_withheld` and
//! `m8_feed_local_owner_only`; the repository's half is in `koru-ai-sqlite`.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use koru_ai_core::bots::{
    Bot, BotAccess, BotConfig, BotPage, BotRepository, BotRequestId, DeviceId, UserScopeId,
};
use koru_ai_core::{
    AgentToolKind, AiEvent, AiIntent, AiPersistence, AiProviderId, AiSurface, AiUsage,
    AttemptStatus, ChatId, ChatRole, DocumentId, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlock, MessageBlockId, MessageBlockKind, MessageId, MessageState, ModelSelection,
    PendingTurn, ProfileId, ReasoningChoice, Timestamp, ToolActivity, ToolActivityStatus,
    ToolApproval, ToolInvocationId,
};
use koru_bot_client_wire::{PageExpectation, WireMessage, decode_body, encode_body, entry_kind};
use koru_store_core::transport::TransportMode;
use uuid::Uuid;

use super::*;

/// The Store as the feed asks it: two stored modes per workspace, a fence
/// the feed must never read, and a switch that makes a read fail.
struct Authority {
    owner: BotOwner,
    /// `(the workspace's stored mode, its Profile's stored mode)`.
    stored: Mutex<BTreeMap<WorkspaceId, (TransportMode, TransportMode)>>,
    /// Workspaces fenced in memory: what `transport_admission` would answer
    /// Local only for, whatever is stored.
    fenced: Mutex<BTreeSet<WorkspaceId>>,
    failing: Mutex<bool>,
    asked: Mutex<Vec<WorkspaceId>>,
}

impl Authority {
    fn new(owner: BotOwner) -> Self {
        Self {
            owner,
            stored: Mutex::default(),
            fenced: Mutex::default(),
            failing: Mutex::new(false),
            asked: Mutex::default(),
        }
    }
    fn store(&self, workspace: WorkspaceId, own: TransportMode, profile: TransportMode) {
        self.stored
            .lock()
            .unwrap()
            .insert(workspace, (own, profile));
    }
    fn asked(&self) -> Vec<WorkspaceId> {
        std::mem::take(&mut self.asked.lock().unwrap())
    }
}

impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, _: WorkspaceId) -> Result<ProfileId, BotError> {
        Err(BotError::Forbidden)
    }
    fn page(&self, _: &BotPage) -> Result<(), BotError> {
        Err(BotError::Forbidden)
    }
    fn home_copy(&self, workspace: WorkspaceId) -> Result<HomeCopy, BotError> {
        self.asked.lock().unwrap().push(workspace);
        if *self.failing.lock().unwrap() {
            return Err(BotError::Unavailable);
        }
        // The stored rows alone, as the real adapter reads them. The fence
        // is not consulted.
        let (own, profile) = self
            .stored
            .lock()
            .unwrap()
            .get(&workspace)
            .copied()
            .ok_or(BotError::Unavailable)?;
        Ok(crate::bots::home_copy_of(own, profile))
    }
}

fn at(offset: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + offset)
}

fn bot_of(owner: BotOwner, home: WorkspaceId, name: &str) -> Bot {
    Bot {
        may_ask: true,
        id: BotId::mint(),
        home_workspace: home,
        user_scope: owner.user_scope,
        owner_device: owner.device,
        config: BotConfig {
            name: name.into(),
            role: BotPage {
                workspace: home,
                document: DocumentId::new(),
            },
            working_folder: if cfg!(windows) {
                "D:/disposable"
            } else {
                "/disposable"
            }
            .into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
            avatar: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    }
}

fn pending_turn(chat_id: ChatId, ordinal: u32, offset: i64) -> PendingTurn {
    let user_id = MessageId::new();
    let assistant_id = MessageId::new();
    PendingTurn {
        user_message: Message {
            id: user_id,
            chat_id,
            ordinal,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: format!("Question {ordinal}"),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: Some(at(offset)),
        },
        assistant_message: Message {
            id: assistant_id,
            chat_id,
            ordinal: ordinal + 1,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(AiProviderId::parse("mock").unwrap()),
            requested_model: Some("deterministic-v1".into()),
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: None,
        },
        attempt: InferenceAttempt {
            id: InferenceAttemptId::new(),
            operation_id: Uuid::new_v4(),
            chat_id: Some(chat_id),
            assistant_message_id: Some(assistant_id),
            surface: AiSurface::Standalone,
            intent: AiIntent::Chat,
            provider: AiProviderId::parse("mock").unwrap(),
            requested_model: "deterministic-v1".into(),
            resolved_model: None,
            requested_reasoning: ReasoningChoice::Default,
            effective_reasoning: None,
            prompt_template_version: "mock-v1".into(),
            status: AttemptStatus::Pending,
            queued_at: at(offset),
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: false,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: 1,
            context_document_count: 0,
            context_message_block_count: 0,
            usage: AiUsage::default(),
            context_used_tokens: None,
            context_limit_tokens: None,
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        },
        attachment_ids: Vec::new(),
    }
}

/// One finished turn: the person's message and the Bot's reply.
fn complete_turn(repo: &mut Repository, chat: ChatId, ordinal: u32, reply: &str) -> PendingTurn {
    let offset = i64::from(ordinal) * 10;
    let turn = pending_turn(chat, ordinal, offset);
    repo.begin_turn(&turn).unwrap();
    repo.apply_event(
        &AiEvent::TextDelta {
            attempt_id: turn.attempt.id,
            text: reply.into(),
            at: at(offset + 1),
        },
        &[],
    )
    .unwrap();
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: turn.attempt.id,
            stop_reason: None,
            at: at(offset + 2),
        },
        &[MessageBlock {
            id: MessageBlockId::new(),
            message_id: turn.assistant_message.id,
            ordinal: 0,
            kind: MessageBlockKind::Paragraph,
            markdown: reply.into(),
            plain_text: reply.into(),
        }],
    )
    .unwrap();
    turn
}

/// A database with one Bot of `owner` in a workspace whose stored modes let
/// it be copied, and the authority that says so.
fn fixture() -> (Repository, Bot, Authority) {
    let owner = BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::from_uuid(Uuid::new_v4()),
    };
    let home = WorkspaceId::new();
    let authority = Authority::new(owner);
    authority.store(home, TransportMode::P2p, TransportMode::Cloud);
    let mut repo = Repository::in_memory().unwrap();
    let bot = bot_of(owner, home, "Fixture Bot");
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    (repo, bot, authority)
}

fn started(repo: &mut Repository, authority: &Authority) -> OwnerFeed {
    OwnerFeed::start(repo, authority, OwnerFeed::new_boot_id()).unwrap()
}

/// A linked desktop reading everything: pages from `cursor` until one says
/// it is caught up, each checked as a client checks it and carried through
/// the codec.
fn read_all(
    feed: &mut OwnerFeed,
    repo: &mut Repository,
    authority: &Authority,
    mut cursor: u64,
    reset: bool,
) -> Vec<Changes> {
    let mut pages = Vec::new();
    loop {
        let page = feed.page_with(repo, authority, cursor, reset).unwrap();
        let owner_device = feed.owner().device;
        page.check_page(&PageExpectation {
            owner_device: &owner_device,
            feed_id: feed.feed_id(),
            boot_id: feed.boot_id(),
            cursor,
        })
        .unwrap();
        page.check_limits(&Limits::this_build()).unwrap();
        assert_eq!(page.reset, reset);
        // The page is a frame the protocol carries, byte for byte.
        let body = encode_body(&WireMessage::Changes(page.clone())).unwrap();
        let WireMessage::Changes(decoded) = decode_body(&body).unwrap() else {
            panic!("a page decoded as another message");
        };
        assert_eq!(decoded, page);
        cursor = page.to_cursor;
        let caught_up = page.caught_up;
        pages.push(page);
        if caught_up {
            return pages;
        }
        assert!(pages.len() < 10_000, "the feed never caught up");
    }
}

fn entries(pages: &[Changes]) -> Vec<&Entry> {
    pages.iter().flat_map(|page| page.entries.iter()).collect()
}

fn present(pages: &[Changes], kind: &str) -> usize {
    entries(pages)
        .into_iter()
        .filter(|entry| entry.kind == kind && !entry.removed)
        .count()
}

// ---------------------------------------------------------------- the pages

#[test]
fn m8_feed_pages_a_reset_reads_the_feed_in_cursor_order_and_ends_caught_up_with_counts() {
    let (mut repo, bot, authority) = fixture();
    for index in 0..40u32 {
        complete_turn(&mut repo, bot.main_chat, index * 2, "A reply");
    }
    // The audit is on in this crate's tests too: every one of those commits
    // was looked at, and none was a fixture's.
    let (commits, exempt) = repo.feed_audit_commits();
    assert!(commits > 120, "{commits}");
    assert_eq!(exempt, 0);
    let mut feed = started(&mut repo, &authority);
    assert_eq!(feed.withheld_bots(), 0);
    let head = feed.head(&repo).unwrap();
    // One Bot, one conversation and eighty messages: more than one page.
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].entries.len(), 64);
    assert_eq!(pages[1].entries.len(), 18);
    // Ascending within and across pages, each page from where the last ended.
    let all = entries(&pages);
    assert!(all.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert_eq!(pages[0].from_cursor, 0);
    assert_eq!(pages[1].from_cursor, pages[0].to_cursor);
    assert_eq!(pages[0].to_cursor, pages[0].entries.last().unwrap().seq);
    assert_eq!(pages[1].to_cursor, head);
    // Only the last page is caught up, and only it carries the counts.
    assert!(!pages[0].caught_up && pages[0].live.is_none() && pages[0].withheld_bots.is_none());
    assert!(pages[1].caught_up);
    assert_eq!(
        pages[1].live,
        Some(LiveCounts {
            bots: 1,
            chats: 1,
            messages: 80
        })
    );
    assert_eq!(pages[1].withheld_bots, Some(0));
    assert_eq!(pages[1].head, head);
    // The counts are what the pages delivered.
    assert_eq!(present(&pages, entry_kind::BOT), 1);
    assert_eq!(present(&pages, entry_kind::CHAT), 1);
    assert_eq!(present(&pages, entry_kind::MESSAGE), 80);
    // Each entry names what a copy files it under.
    for entry in &all {
        match entry.kind.as_str() {
            "bot" => {
                assert_eq!(entry.id, bot.id.to_string());
                assert!(entry.bot_id.is_none() && entry.chat_id.is_none());
            }
            "chat" => {
                assert_eq!(entry.id, bot.main_chat.to_string());
                assert_eq!(entry.bot_id, Some(bot.id.to_string()));
                assert!(entry.chat_id.is_none());
            }
            "message" => {
                assert_eq!(entry.bot_id, Some(bot.id.to_string()));
                assert_eq!(entry.chat_id, Some(bot.main_chat.to_string()));
            }
            other => panic!("an unexpected kind: {other}"),
        }
        assert_eq!(entry.record.as_ref().unwrap().format(), REPLICA_FORMAT);
        assert_eq!(entry.record.as_ref().unwrap().kind(), entry.kind);
    }
    // At the head, a page is empty, caught up, and still carries the counts.
    let idle = feed.page_with(&mut repo, &authority, head, false).unwrap();
    assert!(idle.entries.is_empty() && idle.caught_up && !idle.reset);
    assert_eq!(
        (idle.from_cursor, idle.to_cursor, idle.head),
        (head, head, head)
    );
    assert_eq!(idle.live, pages[1].live);
    // A subject that changes while a desktop is between pages appears again.
    let first = feed.page_with(&mut repo, &authority, 0, true).unwrap();
    let seen: BTreeSet<&str> = first
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(seen.contains(bot.id.to_string().as_str()));
    let mut renamed = bot.clone();
    renamed.config.name = "Renamed".into();
    renamed.revision = 2;
    renamed.updated_at_ms = 5;
    repo.save_bot(
        &renamed,
        ProfileId::new(),
        BotRequestId::mint(),
        "update",
        Some(1),
    )
    .unwrap();
    let rest = read_all(&mut feed, &mut repo, &authority, first.to_cursor, true);
    let again: Vec<&Entry> = entries(&rest)
        .into_iter()
        .filter(|entry| entry.kind == entry_kind::BOT)
        .collect();
    assert_eq!(again.len(), 1);
    let Some(Record::Bot(record)) = &again[0].record else {
        panic!("the Bot's entry carries no Bot record");
    };
    assert_eq!(record.bot.config.name, "Renamed");
    assert_eq!(record.bot.revision, 2);
    // The Bot's record holds no state of the team and no coordination state.
    assert!(record.bot.open_operation.is_none() && record.bot.pending_change.is_none());
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_pages_a_pending_message_has_no_text_and_a_tool_call_no_arguments_or_results() {
    let (mut repo, bot, authority) = fixture();
    let turn = pending_turn(bot.main_chat, 0, 0);
    repo.begin_turn(&turn).unwrap();
    repo.apply_event(
        &AiEvent::TextDelta {
            attempt_id: turn.attempt.id,
            text: "streamed-but-not-committed".into(),
            at: at(1),
        },
        &[],
    )
    .unwrap();
    let mut tool = ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: turn.attempt.id,
        chat_id: bot.main_chat,
        assistant_message_id: turn.assistant_message.id,
        tool_name: AgentToolKind::Tool.as_token().into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::NotRequired,
        proposed_at: at(2),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: Some("Run the script".into()),
        agent_summary: Some("May it run?".into()),
        agent_input: Some("argument-with-a-secret".into()),
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    };
    repo.create_tool_activity(&tool).unwrap();
    tool.status = ToolActivityStatus::ApprovalRequired;
    tool.approval = ToolApproval::Pending;
    repo.advance_agent_activity(&tool).unwrap();

    let mut feed = started(&mut repo, &authority);
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    let all = entries(&pages);
    let reply = all
        .iter()
        .find(|entry| entry.id == turn.assistant_message.id.to_string())
        .unwrap();
    let Some(Record::Message(record)) = &reply.record else {
        panic!("the pending reply carries no message record");
    };
    // The pending reply: a placeholder, with its tool call beside it.
    assert_eq!(record.message.state, MessageState::Pending);
    assert_eq!(record.message.markdown, "");
    assert!(record.blocks.is_empty() && record.usage.is_none() && record.sources.is_none());
    assert!(!record.body_omitted);
    assert_eq!(record.tool_activities.len(), 1);
    let sent = &record.tool_activities[0];
    assert_eq!(sent.status, ToolActivityStatus::ApprovalRequired);
    assert_eq!(sent.agent_label.as_deref(), Some("Run the script"));
    assert_eq!(sent.agent_summary.as_deref(), Some("May it run?"));
    assert!(sent.agent_input.is_none() && sent.agent_output.is_none());
    // The conversation is running and needs the person.
    let chat = all
        .iter()
        .find(|entry| entry.kind == entry_kind::CHAT)
        .unwrap();
    let Some(Record::Chat(chat)) = &chat.record else {
        panic!("the conversation carries no chat record");
    };
    assert!(chat.activity.running && chat.activity.needs_you);
    assert!(chat.chat.bot.is_none());
    // On the wire: neither member exists, and neither the streamed text nor
    // the argument is anywhere in any page.
    let wire: String = pages
        .iter()
        .map(|page| {
            String::from_utf8(encode_body(&WireMessage::Changes(page.clone())).unwrap()).unwrap()
        })
        .collect();
    for absent in [
        "agent_input",
        "agent_output",
        "argument-with-a-secret",
        "streamed-but-not-committed",
    ] {
        assert!(!wire.contains(absent), "{absent}");
    }
    assert!(wire.contains("Run the script"));

    // The call finishes with a result, and the reply ends: the text is sent,
    // the result still is not.
    tool.status = ToolActivityStatus::Complete;
    tool.approval = ToolApproval::Approved;
    tool.started_at = Some(at(3));
    tool.completed_at = Some(at(4));
    tool.agent_output = Some("result-with-a-secret".into());
    repo.advance_agent_activity(&tool).unwrap();
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: turn.attempt.id,
            stop_reason: None,
            at: at(5),
        },
        &[],
    )
    .unwrap();
    let cursor = pages.last().unwrap().to_cursor;
    let later = read_all(&mut feed, &mut repo, &authority, cursor, false);
    let wire: String = later
        .iter()
        .map(|page| {
            String::from_utf8(encode_body(&WireMessage::Changes(page.clone())).unwrap()).unwrap()
        })
        .collect();
    assert!(wire.contains("streamed-but-not-committed"));
    for absent in [
        "agent_input",
        "agent_output",
        "result-with-a-secret",
        "argument-with-a-secret",
    ] {
        assert!(!wire.contains(absent), "{absent}");
    }
    // Nothing a page or an entry prints holds what a person or a Bot wrote.
    let printed = format!("{later:?} {:?}", entries(&later));
    assert!(!printed.contains("streamed") && !printed.contains("Run the script"));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_pages_a_page_keeps_the_byte_limit_and_one_record_too_large_goes_without_its_body() {
    let (mut repo, bot, authority) = fixture();
    // Replies of about 150 KB as text and as much again in blocks: three or
    // four fill a page's 524,288 bytes long before 64 entries do.
    let large = "y".repeat(150_000);
    for index in 0..4u32 {
        complete_turn(&mut repo, bot.main_chat, index * 2, &large);
    }
    // One reply past the largest record there is.
    let huge = "z".repeat(900_000);
    complete_turn(&mut repo, bot.main_chat, 8, &huge);
    let mut feed = started(&mut repo, &authority);
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert!(pages.len() >= 3, "{}", pages.len());
    for page in &pages {
        let bytes: usize = page.entries.iter().map(Entry::encoded_len).sum();
        assert!(
            page.entries.len() == 1 || bytes <= limits::MAX_PAGE_BYTES as usize,
            "{} entries, {bytes} bytes",
            page.entries.len()
        );
        assert!(!page.entries.is_empty());
    }
    // Every message arrived exactly once.
    assert_eq!(present(&pages, entry_kind::MESSAGE), 10);
    let mut omitted = 0;
    for entry in entries(&pages) {
        let Some(Record::Message(record)) = &entry.record else {
            continue;
        };
        assert!(entry.encoded_len() <= koru_bot_client_wire::MAX_RECORD_BYTES + 512);
        if record.body_omitted {
            omitted += 1;
            assert_eq!(record.message.markdown, "");
            assert!(record.blocks.is_empty());
            assert_eq!(record.message.state, MessageState::Complete);
        } else if record.message.role == ChatRole::Assistant {
            assert_eq!(record.message.markdown.len(), 150_000);
        }
    }
    assert_eq!(omitted, 1);
    assert_eq!(
        pages.last().unwrap().live,
        Some(LiveCounts {
            bots: 1,
            chats: 1,
            messages: 10
        })
    );
}

#[test]
fn m8_feed_pages_the_writer_budget_yields_to_a_queued_command() {
    let start = Instant::now();
    let mut budget = FeedBudget::default();
    // Nothing built yet: a page may go, queue or not.
    assert!(budget.may_build(start, 0, false));
    assert!(budget.may_build(start, 3, true));
    budget.built(start);
    let after = |millis: u64| start + Duration::from_millis(millis);
    // With a command queued, the feed waits: the command runs first.
    assert!(!budget.may_build(after(0), 1, false));
    assert!(!budget.may_build(after(249), 1, false));
    assert_eq!(
        budget.wait(after(100), 1, false),
        Some(Duration::from_millis(150))
    );
    // After 250 ms it is served anyway, so a busy writer cannot starve a
    // linked desktop.
    assert!(budget.may_build(after(250), 1, false));
    assert!(budget.may_build(after(250), 50, true));
    // With nothing queued it builds at once, outside a reset.
    assert!(budget.may_build(after(0), 0, false));
    assert_eq!(budget.wait(after(0), 0, false), None);
    // During a reset: at most one page per 50 ms, so a long history is read
    // in steps a chat write can fall between.
    assert!(!budget.may_build(after(0), 0, true));
    assert!(!budget.may_build(after(49), 0, true));
    assert_eq!(
        budget.wait(after(20), 0, true),
        Some(Duration::from_millis(30))
    );
    assert!(budget.may_build(after(50), 0, true));
    // A queued command during a reset waits the longer of the two.
    assert!(!budget.may_build(after(60), 1, true));
    assert!(budget.may_build(after(250), 1, true));
    // Each page moves the clock.
    budget.built(after(250));
    assert!(!budget.may_build(after(260), 1, false));
    assert!(budget.may_build(after(500), 1, false));
    // A clock that reads earlier than the last page never panics, and waits.
    assert!(!budget.may_build(start, 1, false));
    assert_eq!(FeedBudget::STARVED_AFTER, Duration::from_millis(250));
    assert_eq!(FeedBudget::RESET_SPACING, Duration::from_millis(50));
    assert_eq!(RECONCILE_EVERY, Duration::from_secs(60));
}

// --------------------------------------------------------------- the starts

#[test]
fn m8_feed_starts_a_subscribe_continues_or_is_reset_with_its_reason() {
    let (mut repo, bot, authority) = fixture();
    complete_turn(&mut repo, bot.main_chat, 0, "A reply");
    let first = started(&mut repo, &authority);
    let device = first.owner().device;
    let head = first.head(&repo).unwrap();
    let gap = |reason| {
        Subscription::Reset(Gap {
            owner_device: device,
            reason,
            head,
        })
    };
    // The first time, and a desktop that asks to be sent everything again.
    let fresh = Subscribe::fresh(device);
    assert_eq!(
        first.subscribe(&repo, &fresh, false),
        Ok(gap(GapReason::FirstSubscribe))
    );
    assert_eq!(
        first.subscribe(&repo, &fresh, true),
        Ok(gap(GapReason::ClientRequested))
    );
    // A cursor this start served continues, at the head and below it.
    let resume = |feed: &str, boot: &str, cursor: u64| Subscribe {
        owner_device: device,
        feed_id: Some(feed.to_owned()),
        boot_id: Some(boot.to_owned()),
        cursor: Some(cursor),
    };
    for cursor in [0, 1, head] {
        assert_eq!(
            first.subscribe(
                &repo,
                &resume(first.feed_id(), first.boot_id(), cursor),
                false
            ),
            Ok(Subscription::Continue { cursor })
        );
    }
    // A cursor past the head, an unknown start, another feed.
    assert_eq!(
        first.subscribe(
            &repo,
            &resume(first.feed_id(), first.boot_id(), head + 1),
            false
        ),
        Ok(gap(GapReason::FeedRewound))
    );
    assert_eq!(
        first.subscribe(&repo, &resume(first.feed_id(), &"0".repeat(32), 0), false),
        Ok(gap(GapReason::FeedRewound))
    );
    assert_eq!(
        first.subscribe(&repo, &resume(&"f".repeat(32), first.boot_id(), 0), false),
        Ok(gap(GapReason::FeedReplaced))
    );
    // Another computer's name, and a message with only some members.
    assert_eq!(
        first.subscribe(
            &repo,
            &Subscribe::fresh(DeviceId::from_uuid(Uuid::new_v4())),
            false
        ),
        Err(FeedRefusal::WrongOwner)
    );
    assert_eq!(
        first.subscribe(
            &repo,
            &Subscribe {
                owner_device: device,
                feed_id: Some(first.feed_id().to_owned()),
                boot_id: None,
                cursor: Some(0),
            },
            false
        ),
        Err(FeedRefusal::Malformed)
    );

    // The worker restarts: a new start, the same feed. Every cursor the old
    // start served still continues, so an ordinary restart resets nobody.
    let (old_feed, old_boot) = (first.feed_id().to_owned(), first.boot_id().to_owned());
    drop(first);
    let second = started(&mut repo, &authority);
    assert_eq!(second.feed_id(), old_feed);
    assert_ne!(second.boot_id(), old_boot);
    assert_eq!(second.boot_id().len(), 32);
    assert_eq!(
        second.subscribe(&repo, &resume(&old_feed, &old_boot, head), false),
        Ok(Subscription::Continue { cursor: head })
    );
    // And it is sent what happened since, in pages that name the new start.
    complete_turn(&mut repo, bot.main_chat, 2, "After the restart");
    let mut second = second;
    let page = second
        .page_with(&mut repo, &authority, head, false)
        .unwrap();
    assert_eq!(page.boot_id, second.boot_id());
    assert_eq!(page.from_cursor, head);
    assert!(page.caught_up && page.entries.len() == 3);
    // The old start never served anything above where the new one began.
    assert_eq!(
        second.subscribe(&repo, &resume(&old_feed, &old_boot, page.to_cursor), false),
        Ok(Subscription::Reset(Gap {
            owner_device: device,
            reason: GapReason::FeedRewound,
            head: page.to_cursor,
        }))
    );
}

// ------------------------------------------------- who is never sent a record

/// What every withheld case must show: the Bot, its conversation and its
/// messages are sent as removed, counted nowhere but in `withheld_bots`.
fn assert_withheld(pages: &[Changes], withheld: u32) {
    let all = entries(pages);
    assert!(!all.is_empty());
    assert!(
        all.iter()
            .all(|entry| entry.removed && entry.record.is_none()),
        "{all:?}"
    );
    let last = pages.last().unwrap();
    assert_eq!(
        last.live,
        Some(LiveCounts {
            bots: 0,
            chats: 0,
            messages: 0
        })
    );
    assert_eq!(last.withheld_bots, Some(withheld));
}

#[test]
fn m8_feed_withheld_a_workspace_or_its_profile_stored_local_only_is_sent_as_removed_and_counted() {
    for (own, profile) in [
        (TransportMode::LocalOnly, TransportMode::Cloud),
        (TransportMode::P2p, TransportMode::LocalOnly),
        (TransportMode::LocalOnly, TransportMode::LocalOnly),
    ] {
        let (mut repo, bot, authority) = fixture();
        complete_turn(&mut repo, bot.main_chat, 0, "kept-on-this-computer");
        authority.store(bot.home_workspace, own, profile);
        // The start reconcile finishes before the first page: when the feed
        // exists the markers already say "withheld".
        let mut feed = started(&mut repo, &authority);
        assert_eq!(feed.withheld_bots(), 1);
        let homes = feed.reconcile_homes(&repo).unwrap();
        assert_eq!(homes.len(), 1);
        assert!(homes[0].local && homes[0].withheld);
        // So the very first page build needs no reconcile.
        let copies = HomeCopies::ask(&authority, [bot.home_workspace]);
        assert!(matches!(
            feed.page(&repo, 0, true, &copies).unwrap(),
            PageOutcome::Page(_)
        ));
        let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
        assert_withheld(&pages, 1);
        assert_eq!(entries(&pages).len(), 4);
        // No name, no text and no path of it is in any page.
        let wire: String = pages
            .iter()
            .map(|page| {
                String::from_utf8(encode_body(&WireMessage::Changes(page.clone())).unwrap())
                    .unwrap()
            })
            .collect();
        for absent in [
            "Fixture Bot",
            "kept-on-this-computer",
            "disposable",
            "Question",
        ] {
            assert!(!wire.contains(absent), "{absent}");
        }
        // A command of a linked desktop that names it is told it is not
        // there, like a Bot that never existed.
        assert_eq!(
            feed.command_target(&repo, &authority, bot.id),
            Err(BotError::NotFound)
        );
        assert_eq!(
            feed.command_target(&repo, &authority, BotId::mint()),
            Err(BotError::NotFound)
        );
        assert!(repo.feed_audit_refusals().is_empty());
    }
}

#[test]
fn m8_feed_withheld_no_record_is_built_when_the_marker_still_says_not_withheld() {
    let (mut repo, bot, authority) = fixture();
    complete_turn(&mut repo, bot.main_chat, 0, "kept-on-this-computer");
    let mut feed = started(&mut repo, &authority);
    assert_eq!(feed.withheld_bots(), 0);
    assert_eq!(feed.command_target(&repo, &authority, bot.id), Ok(()));
    // The person keeps the workspace on this computer. Nothing told the
    // worker: every marker still says the Bot may be sent.
    authority.store(
        bot.home_workspace,
        TransportMode::LocalOnly,
        TransportMode::Cloud,
    );
    assert!(!feed.reconcile_homes(&repo).unwrap()[0].withheld);
    authority.asked();

    // The page asks the Store before it builds anything, once for the one
    // workspace on the page.
    let PageOutcome::Ask(homes) = feed.page(&repo, 0, true, &HomeCopies::default()).unwrap() else {
        panic!("a page was built before the Store was asked");
    };
    assert_eq!(homes, [bot.home_workspace]);
    let copies = HomeCopies::ask(&authority, homes);
    assert_eq!(authority.asked(), [bot.home_workspace]);
    assert!(!copies.is_copied(bot.home_workspace));
    // With the answer, no page is built at all: the difference is
    // reconciled first.
    assert!(matches!(
        feed.page(&repo, 0, true, &copies).unwrap(),
        PageOutcome::Reconcile
    ));
    // And the repository builds no record of it whatever the marker says.
    let scan = repo
        .feed_scan(feed.owner(), 0, 64, usize::MAX, &BTreeSet::new())
        .unwrap();
    assert!(scan.items.iter().all(|item| !item.marker.withheld));
    assert!(
        scan.items
            .iter()
            .all(|item| matches!(item.body, koru_ai_sqlite::FeedBody::Removed))
    );

    // The pages a desktop is sent: the page build found the difference, the
    // reconcile ran, and the Bot is removed from the copy.
    let head = feed.head(&repo).unwrap();
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert_withheld(&pages, 1);
    assert_eq!(entries(&pages).len(), 4);
    assert!(entries(&pages).iter().all(|entry| entry.seq > head));
    assert_eq!(feed.withheld_bots(), 1);
    assert_eq!(
        feed.command_target(&repo, &authority, bot.id),
        Err(BotError::NotFound)
    );
    // The Store is asked once per distinct workspace on a page, not per
    // entry: one Bot, four entries, one question per page build.
    authority.asked();
    let page = feed.page_with(&mut repo, &authority, 0, true).unwrap();
    assert_eq!(page.entries.len(), 4);
    assert_eq!(authority.asked(), [bot.home_workspace]);
}

#[test]
fn m8_feed_withheld_a_failed_read_withholds_and_a_fence_alone_does_not() {
    let (mut repo, bot, authority) = fixture();
    complete_turn(&mut repo, bot.main_chat, 0, "A reply");
    let mut feed = started(&mut repo, &authority);
    let head = feed.head(&repo).unwrap();

    // The workspace is fenced in memory. Its stored mode says it is copied,
    // and the stored mode is all the feed reads: nothing changes.
    authority.fenced.lock().unwrap().insert(bot.home_workspace);
    assert_eq!(
        feed.reconcile_with(&mut repo, &authority).unwrap(),
        Reconciled::default()
    );
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert_eq!(present(&pages, entry_kind::BOT), 1);
    assert_eq!(present(&pages, entry_kind::MESSAGE), 2);
    assert_eq!(pages.last().unwrap().withheld_bots, Some(0));
    assert_eq!(feed.head(&repo).unwrap(), head);
    assert_eq!(feed.command_target(&repo, &authority, bot.id), Ok(()));

    // The Store does not answer. The Bot is withheld for as long as that
    // lasts: removed from the copies, counted, and not found by a command.
    *authority.failing.lock().unwrap() = true;
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert_withheld(&pages, 1);
    assert!(entries(&pages).iter().all(|entry| entry.seq > head));
    assert_eq!(
        feed.command_target(&repo, &authority, bot.id),
        Err(BotError::NotFound)
    );
    // An authority that does not implement the read copies nothing either.
    struct Silent(BotOwner);
    impl BotAuthority for Silent {
        fn owner(&self) -> Result<BotOwner, BotError> {
            Ok(self.0)
        }
        fn workspace(&self, _: WorkspaceId) -> Result<ProfileId, BotError> {
            Err(BotError::Forbidden)
        }
        fn page(&self, _: &BotPage) -> Result<(), BotError> {
            Err(BotError::Forbidden)
        }
    }
    assert_eq!(
        Silent(feed.owner()).home_copy(bot.home_workspace),
        Err(BotError::Unavailable)
    );
    assert!(
        !HomeCopies::ask(&Silent(feed.owner()), [bot.home_workspace]).is_copied(bot.home_workspace)
    );

    // The Store answers again. The next reconcile, which the worker runs at
    // every subscribe and once a minute, finds it: the Bot is sent again,
    // whole.
    *authority.failing.lock().unwrap() = false;
    let cursor = pages.last().unwrap().to_cursor;
    assert_eq!(
        feed.reconcile_with(&mut repo, &authority).unwrap(),
        Reconciled {
            changed: 1,
            withheld_bots: 0
        }
    );
    let pages = read_all(&mut feed, &mut repo, &authority, cursor, false);
    assert_eq!(present(&pages, entry_kind::BOT), 1);
    assert_eq!(present(&pages, entry_kind::CHAT), 1);
    assert_eq!(present(&pages, entry_kind::MESSAGE), 2);
    assert_eq!(pages.last().unwrap().withheld_bots, Some(0));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_withheld_the_change_both_ways_rewrites_the_markers_of_that_bot_alone() {
    let (mut repo, bot, authority) = fixture();
    let owner = authority.owner;
    // A second Bot of the same person in another workspace.
    let other_home = WorkspaceId::new();
    authority.store(other_home, TransportMode::Cloud, TransportMode::Cloud);
    let other = bot_of(owner, other_home, "Other Bot");
    repo.save_bot(
        &other,
        ProfileId::new(),
        BotRequestId::mint(),
        "create-other",
        None,
    )
    .unwrap();
    complete_turn(&mut repo, bot.main_chat, 0, "First Bot's reply");
    complete_turn(&mut repo, other.main_chat, 0, "Other Bot's reply");
    let mut feed = started(&mut repo, &authority);
    let pages = read_all(&mut feed, &mut repo, &authority, 0, true);
    assert_eq!(present(&pages, entry_kind::BOT), 2);
    assert_eq!(present(&pages, entry_kind::MESSAGE), 4);
    let cursor = pages.last().unwrap().to_cursor;

    // The first Bot's workspace is kept on this computer.
    authority.store(
        bot.home_workspace,
        TransportMode::LocalOnly,
        TransportMode::Cloud,
    );
    assert_eq!(
        feed.reconcile_with(&mut repo, &authority).unwrap(),
        Reconciled {
            changed: 1,
            withheld_bots: 1
        }
    );
    // Its markers moved, the Bot first; the other Bot's did not.
    let pages = read_all(&mut feed, &mut repo, &authority, cursor, false);
    let all = entries(&pages);
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].kind, entry_kind::BOT);
    assert_eq!(all[0].id, bot.id.to_string());
    assert!(all.iter().all(|entry| entry.removed));
    assert!(all.iter().all(
        |entry| entry.id != other.id.to_string() && entry.bot_id != Some(other.id.to_string())
    ));
    assert_eq!(
        pages.last().unwrap().live,
        Some(LiveCounts {
            bots: 1,
            chats: 1,
            messages: 2
        })
    );
    assert_eq!(pages.last().unwrap().withheld_bots, Some(1));
    // A second reconcile finds nothing to do.
    assert_eq!(
        feed.reconcile_with(&mut repo, &authority).unwrap(),
        Reconciled {
            changed: 0,
            withheld_bots: 1
        }
    );
    // Work of the withheld Bot goes on, and none of it is sent.
    complete_turn(&mut repo, bot.main_chat, 2, "Written while withheld");
    let cursor = pages.last().unwrap().to_cursor;
    let pages = read_all(&mut feed, &mut repo, &authority, cursor, false);
    assert!(entries(&pages).iter().all(|entry| entry.removed));
    assert_eq!(pages.last().unwrap().live.unwrap().messages, 2);

    // The workspace is copied again: the Bot comes back whole, with what it
    // wrote meanwhile.
    authority.store(bot.home_workspace, TransportMode::P2p, TransportMode::Cloud);
    let cursor = pages.last().unwrap().to_cursor;
    assert_eq!(
        feed.reconcile_with(&mut repo, &authority).unwrap(),
        Reconciled {
            changed: 1,
            withheld_bots: 0
        }
    );
    let pages = read_all(&mut feed, &mut repo, &authority, cursor, false);
    let all = entries(&pages);
    assert_eq!(all.len(), 6);
    assert_eq!(all[0].kind, entry_kind::BOT);
    assert!(all.iter().all(|entry| !entry.removed));
    assert_eq!(present(&pages, entry_kind::MESSAGE), 4);
    assert_eq!(
        pages.last().unwrap().live,
        Some(LiveCounts {
            bots: 2,
            chats: 2,
            messages: 6
        })
    );
    assert_eq!(pages.last().unwrap().withheld_bots, Some(0));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_local_owner_only_bot_rows_that_name_another_device_or_person_produce_no_record() {
    let (mut repo, bot, authority) = fixture();
    complete_turn(&mut repo, bot.main_chat, 0, "another-computer's-reply");
    // The same `ai.db` on another device, and under another person: what a
    // copied or restored database looks like to the worker that opens it.
    for other in [
        BotOwner {
            user_scope: authority.owner.user_scope,
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        BotOwner {
            user_scope: UserScopeId::new(),
            device: authority.owner.device,
        },
    ] {
        let here = Authority::new(other);
        // Even with the workspace copied, as far as this Store says.
        here.store(
            bot.home_workspace,
            TransportMode::Cloud,
            TransportMode::Cloud,
        );
        let mut feed = started(&mut repo, &here);
        assert_eq!(feed.owner(), other);
        // Not this person's Bot on this computer: not counted as withheld
        // from anybody, and never asked about.
        assert_eq!(feed.withheld_bots(), 0);
        assert!(here.asked().is_empty());
        let pages = read_all(&mut feed, &mut repo, &here, 0, true);
        let all = entries(&pages);
        assert_eq!(all.len(), 4);
        assert!(
            all.iter()
                .all(|entry| entry.removed && entry.record.is_none())
        );
        assert_eq!(
            pages.last().unwrap().live,
            Some(LiveCounts {
                bots: 0,
                chats: 0,
                messages: 0
            })
        );
        assert_eq!(pages.last().unwrap().withheld_bots, Some(0));
        // Every page is served under this computer's name, never the one
        // the rows carry.
        assert!(pages.iter().all(|page| page.owner_device == other.device));
        let wire: String = pages
            .iter()
            .map(|page| {
                String::from_utf8(encode_body(&WireMessage::Changes(page.clone())).unwrap())
                    .unwrap()
            })
            .collect();
        for absent in ["Fixture Bot", "another-computer's-reply", "disposable"] {
            assert!(!wire.contains(absent), "{absent}");
        }
        assert_eq!(
            feed.command_target(&repo, &here, bot.id),
            Err(BotError::NotFound)
        );
        // Back on its own computer the Bot is sent again, whole.
        let mut home = started(&mut repo, &authority);
        let pages = read_all(&mut home, &mut repo, &authority, 0, true);
        assert_eq!(present(&pages, entry_kind::BOT), 1);
        assert_eq!(present(&pages, entry_kind::MESSAGE), 2);
        assert_eq!(home.command_target(&repo, &authority, bot.id), Ok(()));
    }
    assert!(repo.feed_audit_refusals().is_empty());
}
