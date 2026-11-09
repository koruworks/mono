// SPDX-License-Identifier: MPL-2.0

//! Reusable Koru AI chat and provider-setup surface.
//!
//! The surface owns pixels and interaction state. All service work crosses the
//! bounded background client in [`reader`]; the winit thread never discovers a
//! gateway, reads Store, opens SQLite, or calls a provider.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_ai_core::{
    AiErrorClass, AiProviderId, AiSurface, AiUsage, Chat, ChatRole, ContextRef,
    DocumentDestination, DocumentExportId, InferenceAttemptId, MessageState, ModelSelection,
    ReasoningChoice, ReasoningControl, SourceLink, ToolActivity, ToolActivityStatus,
    ToolInvocationId, ToolMode, UsageReport, WebSearchPhase, WorkspaceId,
};

pub use chats::ChatElsewhere;
/// Re-exported so a host can name the id [`ChatSurface::open_link`] and
/// [`ChatSurface::recent_chats`] carry without taking a `koru-ai-core`
/// dependency of its own.
pub use koru_ai_core::ChatId;
pub use koru_ai_core::DocumentId;
/// Re-exported so a host can open a chat at one of its blocks — a chat link
/// or a search hit — without a `koru-ai-core` dependency of its own.
pub use koru_ai_core::MessageBlockId;
/// Re-exported so a host can hold the message a person last saw without a
/// `koru-ai-core` dependency of its own (D83).
pub use koru_ai_core::MessageId;
/// Re-exported so a host names its window's profile
/// ([`ChatSurface::set_profile`]) without a `koru-ai-core` dependency.
pub use koru_ai_core::ProfileId;
use koru_ai_wire::{
    BranchChatRequest, ChatTranscript, CreateChatRequest, ExportChatRequest, ModelPullEvent,
    ProviderSettingsResponse, ProviderValidationResponse, ProviderValidationStatus, PublicAiEvent,
    PullOllamaModelRequest, RenameChatRequest, RetryTurnRequest, SelectProviderRequest,
    SendTurnRequest, UpdateChatModelRequest, UpdateChatToolPolicyRequest,
    UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use koru_draw::tween::Tween;
use koru_draw::{scene, theme::role};
use koru_ui::button::{ActionButton, ActionStyle, ButtonRole};
use koru_ui::choice::{Choice, ChoiceOutcome};
use koru_ui::confirm::{Confirm, Outcome as ConfirmOutcome};
use koru_ui::context_chip::{ContextChip, ContextState};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::logo::KoruLogo;
use koru_ui::split_button::SplitButton;
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use koru_ui::{Menu, PulsingDots, TextArea, Typed};
use parley::Layout;
use parley::editing::{Cursor, Selection};
use parley::layout::Affinity;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// One line of the composer's text: the app's text line at the current
/// density ([`theme::DocMetrics::chat`]). The document text size does not
/// reach the chat, so this does not follow it.
pub(crate) fn chat_line_height() -> f32 {
    theme::DocMetrics::chat().line
}

/// A run of the chat's own text — a message, or what stands in for one —
/// wrapped at `max_w`: the app's size, in the chat font
/// ([`theme::chat_stack`], the app font until a person chooses another).
pub(crate) fn chat_text_layout(engine: &mut TextEngine, text: &str, max_w: f32) -> Layout<Brush> {
    engine.simple_layout_wrapped_in(text, role::ui(), theme::chat_stack(), max_w)
}

/// Paint only the glyph through a transform, so its button and hit area stay still.
fn paint_turning_caret(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    size: f32,
    turn: f32,
    color: vello::peniko::Color,
) {
    let layout = engine.icon_layout(icons::CARET_DOWN, size);
    let x = rect.x + (rect.w - layout.width()) * 0.5;
    let y = rect.y + (rect.h - layout.height()) * 0.5;
    let turned = root
        * Affine::rotate_about(
            (turn.clamp(0.0, 1.0) * std::f32::consts::PI) as f64,
            (
                (x + layout.width() * 0.5) as f64,
                (y + layout.height() * 0.5) as f64,
            ),
        );
    scene::draw_layout(canvas, turned, &layout, x, y, Some(color));
}

mod agents;
mod approval_modes;
mod attachments;
mod chat_status;
mod chats;
mod composer;
mod composer_band;
mod composer_plus;
mod composer_shell;
mod dictation;
mod doc_links;
mod effort;
mod layout_cache;
mod link_card;
mod live_markdown;
mod microphone;
mod model_picker;
mod new_divider;
mod privacy;
mod questions;
mod queue;
mod quote_pills;
mod reader;
mod restored_scroll;
mod skills;
mod sources;
mod tool_groups;
mod tool_output;
mod usage;
mod work_view;

pub use agents::{AgentDefaults, agent_key, provider_mark};
pub use approval_modes::NewChatPermission;
pub use attachments::ViewerLift;
pub use composer::{ChatComposer, ComposedMessage};
pub use composer_plus::ComputerPickerKind;
pub use composer_plus::WorkspaceImportRequest;
pub use dictation::{DictationPress, DictationShown};
pub use microphone::{MicrophoneAsk, MicrophoneDevice, Microphones};
pub use model_picker::FavoriteModel;

use agents::{AgentStatus, agent_chip_label, agent_row_body, agent_row_title, is_agent_row};
use koru_ai_core::AGENT_DEFAULT_MODEL;
use layout_cache::{BlockLayout, MessageInput, MessageLayout, RebuildStats};
use quote_pills::{AddPill, ResponseMenuItem};
use reader::{Command, DestinationOption, POLL_INTERVAL, Reader, SendError, Update};
use usage::{format_usage_report, message_content_height, message_height, usage_line};

const PAD: f32 = 14.0;
const FIELD_H: f32 = 36.0;
const BUTTON_H: f32 = ActionButton::H;
const CARD_GAP: f32 = 10.0;
/// The space under an agent's notice line (D69), as under a work line.
const NOTICE_GAP: f32 = 6.0;
const COMPOSER_GAP: f32 = 8.0;
const COMPOSER_MIN_LINES: usize = 1;
const COMPOSER_MAX_LINES: usize = 8;
const MIN_COMPOSER_W: f32 = 80.0;
const HISTORY_ROW_H: f32 = 40.0;
const MODEL_VISIBLE_ROWS: usize = 8;
const ACTION_H: f32 = 28.0;
const BLOCK_GAP: f32 = 8.0;
const USAGE_GAP: f32 = 4.0;
const HIGHLIGHT_FOR: Duration = Duration::from_secs(2);
const CONVERSATION_SCROLL_SPEED: f32 = 1.8;
/// ↓'s diameter (D77), its gap above the composer's edge of the transcript,
/// its arrow, and how far from the end the transcript may sit and still
/// count as at it.
const JUMP_D: f32 = 32.0;
const JUMP_GAP: f32 = 12.0;
const JUMP_ICON: f32 = 16.0;
/// **How far the conversation's end must be out of sight before ↓ shows**,
/// as a share of the conversation viewport's height: the bottom of the last
/// item has to sit more than this far below the composer's top edge. A last
/// line just tucked under the composer is a glance away, not something to
/// jump to (the user: *only show the go to bottom when the last item in the
/// chat crosses the composer by 20% viewport size*). `Ctrl+End` jumps
/// whatever this says.
const JUMP_THRESHOLD: f32 = 0.2;
/// Between the end of *Stored “…” in Koru.* and its *Open* (D78).
const STORED_OPEN_GAP: f32 = 6.0;
const CONTENT_MAX_W: f32 = 800.0;
const USER_MESSAGE_MAX_FRACTION: f32 = 0.70;
/// The composer's toolbar row — the private mark, the spaces, the mode, the
/// model chip and the send button: one control tall.
const COMPOSER_TOOLBAR_H: f32 = BUTTON_H;
/// **The composer's inner padding**, one constant per side, read by the
/// layout ([`ChatSurface::composer_rect`]), by the editor's placement inside
/// the box ([`composer`]), and by every control and hit-test that derives
/// from the box, so none of them can disagree.
///
/// The text sits `COMPOSER_PAD_X` in from the left and right and
/// `COMPOSER_PAD_TOP` below the top; the toolbar row is inset by the same
/// `COMPOSER_PAD_X` and sits `COMPOSER_PAD_BOTTOM` above the bottom, with
/// `COMPOSER_TEXT_GAP` between the last line and the row. It used to be the
/// chrome field padding, 7 px, which left the words flush with the border.
const COMPOSER_PAD_X: f32 = 20.0;
const COMPOSER_PAD_TOP: f32 = 20.0;
const COMPOSER_TEXT_GAP: f32 = 16.0;
const COMPOSER_PAD_BOTTOM: f32 = 14.0;
/// Clear the shared raised shadow's 20 px spread and 3 px downward offset.
const COMPOSER_BOTTOM_MARGIN: f32 = 24.0;
/// **The quiet row under the composer box**, left-aligned with it: an agent
/// chat's folder, small and dim, the way a checkout is named under a prompt.
/// `COMPOSER_FOOTER_GAP` below the box, `COMPOSER_FOOTER_H` tall; a chat with
/// nothing to show there gives it no room at all
/// ([`ChatSurface::composer_footer_rect`]).
const COMPOSER_FOOTER_GAP: f32 = 4.0;
const COMPOSER_FOOTER_H: f32 = 22.0;
/// How long a first Escape during a turn waits for the second that stops
/// it. Past it the hint goes away and the next Escape starts over.
const STOP_ESCAPE_WINDOW: Duration = Duration::from_millis(1500);
/// Between an empty chat's greeting and the composer under it.
const GREETING_GAP: f32 = 20.0;
/// Where the setup fields start: below four provider cards.
const SETUP_FIELDS_Y: f32 = 330.0;
/// The title a chat is made with, until its first message names it.
const DEFAULT_CHAT_TITLE: &str = "New chat";
/// The greetings an empty chat opens on, one picked at random per chat.
const GREETINGS: [koru_i18n::Key; 20] = [
    koru_i18n::Key::AiGreeting01,
    koru_i18n::Key::AiGreeting02,
    koru_i18n::Key::AiGreeting03,
    koru_i18n::Key::AiGreeting04,
    koru_i18n::Key::AiGreeting05,
    koru_i18n::Key::AiGreeting06,
    koru_i18n::Key::AiGreeting07,
    koru_i18n::Key::AiGreeting08,
    koru_i18n::Key::AiGreeting09,
    koru_i18n::Key::AiGreeting10,
    koru_i18n::Key::AiGreeting11,
    koru_i18n::Key::AiGreeting12,
    koru_i18n::Key::AiGreeting13,
    koru_i18n::Key::AiGreeting14,
    koru_i18n::Key::AiGreeting15,
    koru_i18n::Key::AiGreeting16,
    koru_i18n::Key::AiGreeting17,
    koru_i18n::Key::AiGreeting18,
    koru_i18n::Key::AiGreeting19,
    koru_i18n::Key::AiGreeting20,
];
/// How long a title made from a first message may be, in characters.
const AUTO_TITLE_CHARS: usize = 48;
const TOOL_ACTIVITY_MIN_EXPANDED_H: f32 = 60.0;
const TOOL_ACCESS_WORKSPACE_ROW_H: f32 = 32.0;
const TOOL_ACCESS_VISIBLE_ROWS: usize = 6;
/// The workspace-access chip at its widest; a longer label is elided.
const TOOL_ACCESS_CHIP_MAX_W: f32 = 140.0;

/// Where every attempt says it was made. The surface is hosted inside
/// Koru — the context pane or a chat tab — so it is always the context
/// pane on the wire. `AiSurface::Standalone` stays readable in stored
/// attempts from the retired Koru AI window; nothing produces it any more.
const SURFACE: AiSurface = AiSurface::ContextPane;

/// A capability that must remain outside the public AI HTTP contract.
///
/// The desktop host consumes this immediately and writes or removes the OS
/// vault entry. A carried secret must never be logged, persisted, cloned, or
/// serialized.
pub enum OpenAiCredentialRequest {
    Save(String),
    Clear,
}

impl OpenAiCredentialRequest {
    /// `Some(secret)` saves/replaces the key; `None` removes it. Taking the
    /// string leaves only an empty allocation for `Drop` to clear.
    pub fn into_secret(mut self) -> Option<String> {
        match &mut self {
            Self::Save(secret) => Some(std::mem::take(secret)),
            Self::Clear => None,
        }
    }
}

impl Drop for OpenAiCredentialRequest {
    fn drop(&mut self) {
        // Best-effort clearing of the String's initialized bytes. The host
        // should consume this immediately into the provider's zeroizing type.
        if let Self::Save(secret) = self {
            unsafe { secret.as_bytes_mut().fill(0) };
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    None,
    Composer,
    /// The send half of an agent chat's split button.
    SendArrow,
    /// The send button's independently enabled approval-mode menu target.
    ModeChevron,
    /// A reply's *Worked for …* header: Enter or Space opens it.
    Work(MessageId),
    /// An agent's waiting questions (koru-ai D108): the card's own cursor
    /// says which option, box or action.
    Questions(ToolInvocationId),
    WorkspaceSearch,
    OpenAiKey,
    OpenAiModel,
    OllamaEndpoint,
    OllamaModel,
    SystemPrompt,
}

/// A folder pick the host performs for an agent chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingFolderRequest {
    pub chat: ChatId,
    /// Where the picker starts: the chat's folder when one was chosen. The
    /// dialog's title is the host's, through `koru-i18n`.
    pub start: Option<std::path::PathBuf>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SetupProvider {
    Koru,
    OpenAi,
    Ollama,
    /// The *Local agents* card: which agent is picked lives in `setup_agent`.
    Agents,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DestructiveAction {
    ClearHistory,
    ClearUsage,
}

enum ResponseAction {
    Copy(String),
    Branch(MessageId),
    Store(MessageId),
    Link(MessageBlockId),
    Retry(MessageId),
}

/// A pending permission request: the one card in a turn, always open, with
/// its choices — never folded into the work view (`work_view`).
struct ToolActivityLayout {
    activity: ToolActivity,
    before_message: Option<MessageId>,
    top: f32,
    title: Layout<Brush>,
    detail: Layout<Brush>,
    expanded_height: f32,
}

/// **A local agent's notice** (koru-ai D69): a status line it said that is
/// not part of its reply — a hook that failed on the person's machine —
/// drawn as one grey 12 px line (wrapped when longer than the column) in
/// front of the reply it came with. Nothing to press.
struct NoticeLayout {
    message: MessageId,
    /// From the top of the conversation; NaN while it is being placed.
    top: f32,
    layout: Layout<Brush>,
}

#[derive(Clone)]
struct ImageSpec {
    url: String,
    alt: String,
}

struct LoadedImage {
    brush: vello::peniko::ImageBrush,
    width: u32,
    height: u32,
}

type ImageResult = Result<LoadedImage, String>;

/// A picture read and decoded on a `koru-ai-image` thread, then kept: one a
/// reply links to on the web, or a file a call read (D38).
struct ImageLoad {
    pending: Option<Arc<Mutex<Option<ImageResult>>>>,
    ready: Option<LoadedImage>,
    failed: bool,
}

impl ImageLoad {
    fn progress(&self) -> tool_output::picture::Loaded {
        use tool_output::picture::Loaded;
        match &self.ready {
            Some(loaded) => Loaded::Ready(loaded.width, loaded.height),
            None if self.failed => Loaded::Failed,
            None => Loaded::Loading,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ResponsePosition {
    message: usize,
    block: Option<usize>,
    byte: usize,
    affinity: Affinity,
}

struct ResponseSelection {
    anchor: ResponsePosition,
    focus: ResponsePosition,
    dragging: bool,
    pressed_link: Option<String>,
}

struct ResponseTextHit {
    position: ResponsePosition,
    selection: Selection,
    link: Option<String>,
}

/// A message sent and not yet accepted: everything the composer held —
/// its text with its chips, quotes and undo, and its attachments.
struct SentMessage {
    chat: ChatId,
    composer: composer::Draft,
    attachments: Vec<attachments::Draft>,
    /// What it said: a queued message refused by the service goes back to
    /// its queue with it (`queue`).
    markdown: String,
    /// It named a document or a skill by chip (D121).
    documents: bool,
    /// It came from the queue, so a refusal sends it back there rather than
    /// into the composer.
    from_queue: bool,
    /// A recalled entry being sent. If refused, it returns to the walk and
    /// Down still recovers the draft that was present before recall.
    recalled_from: Option<(usize, Vec<HistoryInput>)>,
}

/// A send that has not yet appeared in a transcript read. Its identity is
/// filled by the accepted response, so equal text remains two history steps.
#[derive(Clone, Debug)]
struct LocalInput {
    chat: ChatId,
    message_id: Option<MessageId>,
    markdown: String,
    attachments: Vec<koru_ai_core::ChatAttachment>,
}

#[derive(Clone, Debug)]
struct HistoryInput {
    markdown: String,
    attachments: Vec<koru_ai_core::ChatAttachment>,
}

/// The exact composer context whose workspace grants were requested. A late
/// acknowledgement may only release this message, in this chat and workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GrantKey {
    chat: ChatId,
    markdown: String,
    documents: Vec<DocumentId>,
    attachments: Vec<koru_ai_core::ChatAttachmentId>,
    current: Option<WorkspaceId>,
}

/// One AI conversation surface, usable as a window body or in Koru's context
/// column. Constructing it in tests starts no service client.
pub struct ChatSurface {
    viewport: Rect,
    engine: TextEngine,
    root: Option<PathBuf>,
    reader: Option<Reader>,
    waiting: bool,
    settings: Option<ProviderSettingsResponse>,
    chats: Vec<Chat>,
    /// A chat was created, renamed, deleted, re-pointed or answered since the
    /// host last asked ([`ChatSurface::take_chat_list_changed`]).
    chat_list_changed: bool,
    transcript: Option<ChatTranscript>,
    usage_report: Option<UsageReport>,
    live_usage: Option<AiUsage>,
    live_sources: Vec<SourceLink>,
    open_searches: HashSet<String>,
    web_search_armed: bool,
    /// The chip's measured label width from the last draw, so the chip
    /// is as wide as its words (an agent and its model can be long).
    chip_label_w: f32,
    /// The folder control's measured width — its icon, its name and the air
    /// around them — from the last [`ChatSurface::composer_rect`].
    folder_button_w: f32,
    live_reasoning: String,
    reasoning_expanded: bool,
    attempt: Option<InferenceAttemptId>,
    /// The attempt whose stream has sent its terminal event; it stays in
    /// `attempt` until the transcript read that follows replaces it.
    ended_attempt: Option<InferenceAttemptId>,
    pending_assistant_message_id: Option<MessageId>,
    pending_user: Option<String>,
    /// The running reply carries an agent's answers sent as the next
    /// message (koru-ai D108): it follows with no card of the person's —
    /// the answers are read on the set's card.
    answering: bool,
    pulse: Option<PulsingDots>,
    draft: String,
    status: Option<String>,
    validation: Option<ProviderValidationResponse>,
    reasoning: ReasoningChoice,
    validated_ready: bool,
    pull_active: bool,
    pull_status: Option<String>,
    setup_open: bool,
    setup_provider: SetupProvider,
    /// The chip composer: a private Koru document under the editor
    /// ([`composer`]), so `@` puts a document in the text.
    composer: ChatComposer,
    composer_hit_rect: Rect,
    /// Titles for the documents chips name, from the chips this window sent,
    /// the chat's pinned documents, and the reader thread's lookups. Read by
    /// the conversation to draw a chip as a titled link.
    document_titles: HashMap<DocumentId, String>,
    /// Documents whose title has been asked of the reader once already.
    titles_requested: HashSet<DocumentId>,
    /// A chip or a titled link was clicked: the document the host should open.
    open_document_request: Option<koru_link::KoruLink>,
    conversation_hit_rect: Rect,
    /// The workspace-access panel's filter, shared with the leaf that draws
    /// it (D106).
    workspace_search: model_picker::SharedInput,
    /// Asked for a fresh chat before the service answered the boot (a new
    /// tab, the review's note): the boot's snapshot carries the service's
    /// current chat, and a tab that wanted a new one must not take it.
    /// Cleared when the chat it asked for arrives.
    fresh_chat_wanted: bool,
    /// **A new chat not made yet**: the id of the transcript on screen when
    /// it is a stand-in this surface built ([`ChatSurface::begin_unmade_chat`])
    /// rather than one the service holds. Its model, folder, privacy and
    /// workspace access are kept here, on the stand-in, until the first
    /// message is sent, which makes the chat ([`ChatSurface::make_unmade_chat`]);
    /// so a chat nobody wrote in never reaches the Chats list.
    unmade_chat: Option<ChatId>,
    /// A picture pasted into a chat not made yet: the paste is read once the
    /// chat it is stored under exists.
    paste_after_create: Option<ChatId>,
    openai_key: TextInput,
    openai_model: TextInput,
    ollama_endpoint: TextInput,
    ollama_model: TextInput,
    system_prompt: TextArea,
    debug_prompt_enabled: bool,
    focus: Focus,
    pointer: (f32, f32),
    message_layouts: Vec<MessageLayout>,
    tool_activity_layouts: Vec<ToolActivityLayout>,
    /// Each waiting question set's card (koru-ai D108): what is chosen and
    /// typed, by the row's id, while the set waits.
    question_cards: HashMap<ToolInvocationId, questions::Card>,
    /// Each reply's work view, in the order the replies happened.
    work_layouts: Vec<work_view::WorkLayout>,
    /// A local agent's notices, each a grey line in front of its reply
    /// (koru-ai D69), in the order they came.
    notice_layouts: Vec<NoticeLayout>,
    /// The finished replies whose work is opened; every other one is
    /// folded to *Worked for …*. In memory only.
    opened_work: HashSet<MessageId>,
    /// The step groups opened to a line per call, by reply and position.
    opened_step_groups: HashSet<(MessageId, usize)>,
    /// The calls opened to their detail.
    opened_calls: HashSet<ToolInvocationId>,
    /// The calls whose long output is shown in full rather than folded.
    expanded_outputs: HashSet<ToolInvocationId>,
    /// Each opened call's detail as last laid out, under a fingerprint of
    /// what it showed (`tool_output::layout::fingerprint`).
    call_details: HashMap<ToolInvocationId, (u64, Rc<Vec<tool_output::layout::DetailLine>>)>,
    work_hover: Option<work_view::WorkHit>,
    /// Where each call of the running reply began in its draft, noted as
    /// its row arrived, for a row whose stored offset has not come yet.
    live_offsets: HashMap<ToolInvocationId, u32>,
    /// The running answer's closed blocks, drawn as Markdown while it
    /// streams.
    live_answer: live_markdown::LiveAnswer,
    /// When the running reply was accepted, for *Working for 6s*.
    live_turn_started_ms: Option<i64>,
    /// When a live work header last ticked.
    work_ticked_at: Option<Instant>,
    message_width: f32,
    last_rebuild_stats: RebuildStats,
    messages_dirty: bool,
    conversation_scroll: f32,
    conversation_extent: f32,
    conversation_scroll_drag: Option<f32>,
    /// **Following the newest message** (D77): set by ↓ or `Ctrl+End`,
    /// so a reply that streams in keeps the transcript at its end; any
    /// scroll the person makes upward, a question walked to or a highlight
    /// lets go of it. Off otherwise: streaming never pulls a reader to the
    /// tail on its own.
    follow_tail: bool,
    scroll_to_pending_question: bool,
    scroll_loaded_chat_to_bottom: bool,
    pending_question_anchor: Option<f32>,
    question_navigation: Option<usize>,
    question_navigation_anchor: Option<f32>,
    context_document: Option<(DocumentId, String)>,
    dismissed_context_document: Option<DocumentId>,
    /// When true, the next `set_context_document` call is ignored and the
    /// document pill is cleared instead. Used when the host explicitly starts
    /// a new chat and does not want the active tab's document pre-attached.
    suppress_next_document_context: bool,
    context_workspace: Option<WorkspaceId>,
    recent_documents: Vec<(DocumentId, String)>,
    plus_menu: Option<Menu<composer_plus::PlusItem>>,
    computer_picker_request: Option<ComputerPickerKind>,
    computer_file_choice: Option<(std::path::PathBuf, Choice)>,
    workspace_import_request: Option<WorkspaceImportRequest>,
    pending_workspace_import_grant: Option<(uuid::Uuid, WorkspaceImportRequest)>,
    workspace_imports: HashMap<koru_ai_core::ChatAttachmentId, WorkspaceImportRequest>,
    workspace_import_documents: HashMap<koru_ai_core::ChatAttachmentId, DocumentId>,
    completed_workspace_pills: HashMap<ChatId, Vec<(DocumentId, String)>>,
    pending_computer_reference: Option<(ChatId, std::path::PathBuf, koru_ai_core::AttachmentKind)>,
    chip_band: composer_band::ChipBand,
    chip_scroll: f32,
    composer_shell: Option<composer_shell::Shell>,
    /// *Add to chat* over a reply's selection (D88).
    add_pill: AddPill,
    response_menu_anchor: Option<Rect>,
    credential_request: Option<OpenAiCredentialRequest>,
    pending_openai_settings: Option<(Option<String>, ReasoningChoice)>,
    editing_openai_key: bool,
    model_options_open: bool,
    model_option_offset: usize,
    chat_model_open: bool,
    model_chevron: Tween,
    microphone_chevron: Tween,
    send_chevron: Tween,
    send_chevron_reveal: Tween,
    /// The model picker's rail, query, highlight and scroll
    /// ([`model_picker`]), reset each time it opens.
    picker: model_picker::PickerState,
    /// The picker's tooltip: a rail mark, ↻, ↺, a row's full words.
    picker_tooltip: Tooltip,
    tool_access_open: bool,
    tool_mode_open: bool,
    /// The approval-mode list of an agent chat is open (koru-ai D7).
    approval_mode_open: bool,
    /// The row the keyboard is on in the open approval-mode list.
    approval_mode_focus: Option<usize>,
    /// The approval-mode chip's width as last drawn: its icon, name and caret.
    /// The workspace-access chip's width as last measured: its label, caret
    /// and padding (D107).
    tool_access_chip_w: f32,
    /// Each agent's approval modes, from the same listing as its models.
    agent_modes: HashMap<AiProviderId, approval_modes::ListedModes>,
    /// Each agent's listing asked again for its modes, while its chat is
    /// shown and they are not known: how many times, and when last
    /// ([`approval_modes`]).
    modes_asked: HashMap<AiProviderId, (u8, Instant)>,
    /// The agents whose level a begun chat can change, applied from the
    /// next message (koru-ai D70): their listing said so — the session
    /// host is v20. Every other agent chat keeps its level from its start.
    levels_per_turn: HashSet<AiProviderId>,
    /// The agents whose model a begun chat can change to another of the
    /// same agent's, applied from the next message, as `levels_per_turn`
    /// is for the level: their listing said so — the session host is v22.
    /// Every other agent chat keeps the model it started on.
    models_per_turn: HashSet<AiProviderId>,
    /// The coding agents turned off on this machine, by agent key
    /// (`claude`, `codex`, …), as the host's *Agent providers* setting holds
    /// them. Hidden from every choice; a chat already on one keeps it.
    hidden_agents: std::collections::HashSet<String>,
    /// Where a new chat starts, as the host's *Agent providers* holds it.
    agent_defaults: agents::AgentDefaults,
    /// The order the agents are listed in, by key, as the host holds it.
    agent_order: Vec<String>,
    /// The host can open Settings on *Agent providers*, so the picker's rail
    /// ends in a **+** that asks it to. Off until a host says so: a window
    /// with no such page draws no button that would do nothing.
    agent_settings_offered: bool,
    /// The **+** was pressed: the host opens Settings on Agent providers.
    agent_settings_request: bool,
    /// The host can show a path in the file manager (D62). Off until a host
    /// says so; without it a changed file's folder is opened instead.
    reveal_paths_offered: bool,
    /// A changed file's path was pressed for a file not opened directly:
    /// the host shows this in the file manager.
    reveal_path_request: Option<std::path::PathBuf>,
    /// The models starred in the picker (D46), by agent key and model id,
    /// as the host's roaming setting holds them.
    favorite_models: Vec<FavoriteModel>,
    /// A star was pressed: the whole list, for the host to store. A star
    /// shows at once; the host's answer settles it.
    favorite_models_request: Option<Vec<FavoriteModel>>,
    openai_models: Vec<String>,
    ollama_models: Vec<String>,
    /// Each local agent's readiness, from its validation answer.
    agent_status: HashMap<AiProviderId, AgentStatus>,
    /// When the agents' readiness was last asked for, so a running check is
    /// followed up while the picker or the card shows it.
    agent_status_asked: Option<Instant>,
    /// Each agent's listed models, asked for when settings arrive and again
    /// each time the picker opens.
    agent_models: HashMap<AiProviderId, agents::AgentModels>,
    /// The agents whose list has been asked for and not answered yet, so a
    /// second ask waits for the first and a list on screen stays there.
    agent_models_asked: std::collections::HashSet<AiProviderId>,
    /// The agent picked on the *Local agents* card.
    setup_agent: Option<AiProviderId>,
    /// A folder pick the host owes this surface (the native dialog is the
    /// window's, never the surface's).
    working_folder_request: Option<WorkingFolderRequest>,
    response_hover: Option<(usize, u8)>,
    response_message_hover: Option<usize>,
    tool_activity_hover: Option<(ToolInvocationId, u8)>,
    tool_workspace_offset: usize,
    /// A drag of the workspace list's thumb, held this far below its top.
    tool_workspace_drag: Option<f32>,
    copy_feedback: Tooltip,
    /// The card a link in the conversation shows under a resting pointer:
    /// the document's link card, with *open* for its pencil (koru-ai D95).
    link_card: link_card::LinkCard,
    web_search_tooltip: Tooltip,
    response_selection: Option<ResponseSelection>,
    response_menu: Option<Menu<ResponseMenuItem>>,
    /// Pictures by where they come from: a reply's web address, or the
    /// resolved path of a picture a call read (D38).
    images: HashMap<String, ImageLoad>,
    pending_model_prefetch: Option<AiProviderId>,
    retry_link: Option<Rect>,
    ollama_search_link: Option<Rect>,
    /// **A reply just stored as a document** (D78): the status line that
    /// says so, and the document. *Open* follows that line while it is the
    /// one shown.
    stored_document: Option<(String, DocumentId)>,
    /// *Open* where it was last drawn, for the press.
    stored_open_link: Option<Rect>,
    staged_embedded_send: Option<SendTurnRequest>,
    durable_observations: u8,
    destinations: Vec<DestinationOption>,
    workspace_catalog_error: Option<String>,
    selected_destination: usize,
    destination_open: Option<MessageId>,
    pending_link_chat: Option<ChatId>,
    /// **The profile this surface's window is in** (AI schema 13), as its
    /// host says: the chats it lists, the profile a new chat is made in,
    /// and the only profile whose chat it shows. `None` until the host says.
    profile: Option<ProfileId>,
    /// A chat this surface was asked to show that belongs to another
    /// profile, with the block it was to open at: not shown here, for the
    /// host to open in a window of that profile.
    wants_open_elsewhere: Option<ChatElsewhere>,
    highlight: Option<(MessageBlockId, Instant)>,
    confirm: Option<Confirm>,
    destructive_action: Option<DestructiveAction>,
    /// **The first Escape of a turn**, waiting [`STOP_ESCAPE_WINDOW`] for
    /// the second that stops it; the hint above the composer shows while it
    /// waits. A field rather than a clock read so a test can age it.
    stop_armed_at: Option<Instant>,
    /// What this surface sent, by chat, so Up can recall a message before
    /// the transcript that carries it has been read back.
    sent_inputs: Vec<LocalInput>,
    /// Where Up and Down stand in [`ChatSurface::input_history`]; `None`
    /// is the message being typed.
    history_cursor: Option<usize>,
    /// Frozen while walking so an arriving reply cannot retarget an index.
    history_entries: Option<Vec<HistoryInput>>,
    /// The message being typed when Up first left it, whole, for Down to
    /// give back past the newest.
    history_draft: Option<composer::Draft>,
    history_attachment_draft: Option<Vec<attachments::Draft>>,
    history_chat: Option<ChatId>,
    /// Active staged copies, by new id. Late completions after leaving the
    /// walk are removed rather than surfaced in the next draft.
    recall_restages: HashMap<koru_ai_core::ChatAttachmentId, ChatId>,
    recall_retired: HashMap<koru_ai_core::ChatAttachmentId, ChatId>,
    /// The empty chat's greeting: which of [`GREETINGS`], and the chat it
    /// was picked for, so it holds still while shown and a new chat gets a
    /// new one.
    greeting: Option<(Option<ChatId>, usize)>,
    /// Each chat's draft attachments, kept per chat (chat-attachments D15).
    strips: attachments::Strips,
    /// The clipboard, upload and thumbnail threads behind them.
    attachment_work: attachments::Work,
    /// Thumbnails by attachment id: a draft's, then the same picture's on
    /// its sent card; bounded, the least recently drawn going first.
    thumbnails: attachments::ThumbCache,
    /// The tiles of the message just sent, for its card before the
    /// transcript carries it.
    pending_user_tiles: Vec<attachments::Tile>,
    /// The message just sent, whole, until the service accepts it: put
    /// back in the composer if the send does not go.
    sent_message: Option<SentMessage>,
    pending_grants: Option<(GrantKey, uuid::Uuid)>,
    grants_ready: Option<GrantKey>,
    /// A title lookup is with the reader; one at a time, so background
    /// lookups never crowd a person's own command out of its queue.
    titles_in_flight: bool,
    clipboard_reading: bool,
    /// A picture open in the shared viewer (chat-attachments R3).
    viewer: Option<attachments::ChatViewer>,
    /// A picture tile clicked whose bytes are being fetched and decoded:
    /// the chat, the attachment and its name.
    viewer_opening: Option<(ChatId, koru_ai_core::ChatAttachmentId, String)>,
    /// Where the viewer lives when a host lifts it over the window.
    viewer_lift: Option<attachments::ViewerLift>,
    /// The sent pictures' tiles as last drawn, for a click to open one.
    card_tile_hits: Vec<(Rect, attachments::Tile)>,
    /// **Messages queued while a reply runs**, per chat, in memory only
    /// ([`queue`]).
    queues: queue::Queues,
    /// The pending list's icons and the send button name themselves.
    queue_tooltip: Tooltip,
    /// How the reply that just ended ended, and in which chat, until the
    /// transcript read that follows decides what the queue does.
    reply_end: Option<(ChatId, queue::ReplyEnd)>,
    /// A stop-then-send steer is waiting for its cancel to land: the head of
    /// this chat's queue is sent then, and the queue is not paused.
    steer_after_cancel: Option<ChatId>,
    /// A queued item is being sent through the composer's send.
    sending_queued: bool,
    /// A native steer with the service, and the item it carries.
    steering: Option<(ChatId, queue::Queued)>,
    /// Steered messages the service took, shown under the running reply
    /// until the transcript read at its end carries them.
    steered: Vec<(ChatId, String)>,
    /// The person asked for less motion: the light over *Working for …*
    /// holds still and asks for no frames (D83).
    reduced_motion: bool,
    /// How many replies have ended here, for a host noticing an arrival in
    /// a chat nobody is looking at (`chat_status`).
    replies_finished: u32,
    /// The count [`ChatSurface::take_arrival`] last answered for, and
    /// whether a question was waiting then.
    arrivals_reported: u32,
    asked_reported: bool,
    /// Sends the service took and replies that ended here, and the count
    /// [`ChatSurface::take_activity`] last answered for (D85): the host
    /// moves the chat up its Chats list on each.
    activity: u32,
    activity_reported: u32,
    /// `Ctrl+Shift+↑`/`↓` asked for the chat above or below in the Chats
    /// list, where a host with a navigator answers it (D87).
    wants_chat_step: Option<koru_chats_ui::ChatStep>,
    /// The scroll owed to a chat coming back, and the chat it belongs to
    /// (D102, `restored_scroll`).
    restored_scroll: Option<(ChatId, f32)>,
    /// A NEW divider above the first message the person has not seen, and
    /// the chat opened there (D84). Set by the host as the chat comes into
    /// view.
    new_divider: Option<new_divider::NewDivider>,
    /// Voice input in the composer (D103): the microphone, and the
    /// recording row while the host dictates into it (`dictation`).
    dictation: dictation::ComposerDictation,
    /// The agent's skills under `$` in the composer (D121): the list it
    /// last answered and whether it is open (`skills`).
    skills: skills::SkillList,
    lang: koru_i18n::Lang,
}

impl ChatSurface {
    /// The composer, handed to a host routing an ephemeral dictation
    /// composition into this surface. It owns its private document, so it is
    /// driven through `koru_ui::DictationTarget` with a `()` context, or as
    /// `koru_ui::dictation::ComposeInto::Held`.
    pub fn dictation_composer(&mut self) -> &mut ChatComposer {
        &mut self.composer
    }

    /// A document chip, or a document link in the conversation, was clicked.
    /// The host opens the document as a tab.
    pub fn take_open_document_request(&mut self) -> Option<koru_link::KoruLink> {
        self.composer
            .take_open_request()
            .or_else(|| self.open_document_request.take())
    }

    /// Show the provider setup, for a host that arrives here from its own
    /// Settings.
    ///
    /// **Dormant: nothing opens the setup for now.** A chat opens on the
    /// chat view whether or not a provider was ever chosen, and the model is
    /// picked in the composer's picker; the header's settings button, the
    /// boot and a host's *Open Koru AI setup…* no longer come here. The view
    /// and this door stay, unreached, so turning it back on is one call.
    pub fn open_setup(&mut self) {
        self.chat_model_open = false;
        self.tool_access_open = false;
        self.tool_mode_open = false;
        self.approval_mode_open = false;
        self.setup_open = true;
        self.focus = Focus::None;
    }

    /// Whether the provider setup is showing.
    pub fn setup_open(&self) -> bool {
        self.setup_open
    }

    /// Start the message with `document` as a chip, the way `@` puts one in
    /// the text: what a host does when it opens a chat *on* a document — the
    /// page's `…` → *Open in chat*. At send the chip pins the document, like
    /// any chip. Anything already typed stays after it.
    pub fn attach_document(&mut self, document: DocumentId, title: String) -> bool {
        if !title.trim().is_empty() {
            self.document_titles.insert(document, title);
        }
        let chip = format!(
            "[[{}]] ",
            koru_link::document_link(document.as_uuid(), None)
        );
        let typed = self.composer.text();
        let titles = &self.document_titles;
        let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
        self.composer
            .set_message(&format!("{chip}{typed}"), |document| {
                titles
                    .get(&document)
                    .filter(|title| !title.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| untitled.to_owned())
            });
        self.focus = Focus::Composer;
        true
    }

    /// Where the composer is drawn, in the surface's host coordinate space.
    pub fn dictation_anchor(&self) -> Rect {
        self.composer_text_rect()
    }

    pub fn new() -> Self {
        Self::new_at(None)
    }

    pub fn new_at(root: Option<PathBuf>) -> Self {
        let mut engine = TextEngine::new();
        let composer = ChatComposer::new(root.clone());
        let attachment_work = attachments::Work::new(root.clone());
        let workspace_search = model_picker::SharedInput::with_text(&mut engine, role::ui(), "");
        let openai_key = TextInput::new(&mut engine, role::row());
        let openai_model = TextInput::new(&mut engine, role::row());
        let ollama_endpoint =
            TextInput::with_text(&mut engine, role::row(), "http://localhost:11434");
        let ollama_model = TextInput::new(&mut engine, role::row());
        let system_prompt = TextArea::new(&mut engine, role::ui());
        let picker = model_picker::PickerState::new(&mut engine);
        Self {
            viewport: Rect::new(0.0, 0.0, 460.0, 680.0),
            engine,
            root,
            reader: None,
            waiting: false,
            settings: None,
            chats: Vec::new(),
            chat_list_changed: false,
            transcript: None,
            usage_report: None,
            live_usage: None,
            live_sources: Vec::new(),
            open_searches: HashSet::new(),
            web_search_armed: true,
            chip_label_w: 0.0,
            folder_button_w: 0.0,
            live_reasoning: String::new(),
            reasoning_expanded: false,
            attempt: None,
            ended_attempt: None,
            pending_assistant_message_id: None,
            pending_user: None,
            answering: false,
            pulse: None,
            draft: String::new(),
            status: None,
            validation: None,
            reasoning: ReasoningChoice::Default,
            validated_ready: false,
            pull_active: false,
            pull_status: None,
            setup_open: false,
            setup_provider: SetupProvider::OpenAi,
            composer,
            composer_hit_rect: Rect::new(14.0, 626.0, 338.0, 40.0),
            document_titles: HashMap::new(),
            titles_requested: HashSet::new(),
            open_document_request: None,
            conversation_hit_rect: Rect::new(14.0, PAD, 432.0, 500.0),
            workspace_search,
            fresh_chat_wanted: false,
            unmade_chat: None,
            paste_after_create: None,
            openai_key,
            openai_model,
            ollama_endpoint,
            ollama_model,
            system_prompt,
            debug_prompt_enabled: koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG),
            focus: Focus::Composer,
            pointer: (0.0, 0.0),
            message_layouts: Vec::new(),
            tool_activity_layouts: Vec::new(),
            question_cards: HashMap::new(),
            work_layouts: Vec::new(),
            notice_layouts: Vec::new(),
            opened_work: HashSet::new(),
            opened_step_groups: HashSet::new(),
            opened_calls: HashSet::new(),
            expanded_outputs: HashSet::new(),
            call_details: HashMap::new(),
            work_hover: None,
            live_offsets: HashMap::new(),
            live_answer: live_markdown::LiveAnswer::default(),
            live_turn_started_ms: None,
            work_ticked_at: None,
            message_width: 0.0,
            last_rebuild_stats: RebuildStats::default(),
            messages_dirty: true,
            conversation_scroll: 0.0,
            conversation_extent: 0.0,
            conversation_scroll_drag: None,
            follow_tail: false,
            scroll_to_pending_question: false,
            scroll_loaded_chat_to_bottom: false,
            pending_question_anchor: None,
            question_navigation: None,
            question_navigation_anchor: None,
            context_document: None,
            dismissed_context_document: None,
            suppress_next_document_context: false,
            context_workspace: None,
            recent_documents: Vec::new(),
            plus_menu: None,
            computer_picker_request: None,
            computer_file_choice: None,
            workspace_import_request: None,
            pending_workspace_import_grant: None,
            workspace_imports: HashMap::new(),
            workspace_import_documents: HashMap::new(),
            completed_workspace_pills: HashMap::new(),
            pending_computer_reference: None,
            chip_band: composer_band::ChipBand::default(),
            chip_scroll: 0.0,
            composer_shell: None,
            add_pill: AddPill::default(),
            response_menu_anchor: None,
            credential_request: None,
            pending_openai_settings: None,
            editing_openai_key: false,
            model_options_open: false,
            model_option_offset: 0,
            chat_model_open: false,
            model_chevron: Tween::new(theme::GLYPH_MORPH_MS, false),
            microphone_chevron: Tween::new(theme::GLYPH_MORPH_MS, false),
            send_chevron: Tween::new(theme::GLYPH_MORPH_MS, false),
            send_chevron_reveal: Tween::new(theme::GLYPH_MORPH_MS, false),
            picker,
            picker_tooltip: Tooltip::default(),
            tool_access_open: false,
            tool_mode_open: false,
            approval_mode_open: false,
            approval_mode_focus: None,
            tool_access_chip_w: 0.0,
            agent_modes: HashMap::new(),
            modes_asked: HashMap::new(),
            levels_per_turn: HashSet::new(),
            models_per_turn: HashSet::new(),
            hidden_agents: std::collections::HashSet::new(),
            agent_defaults: agents::AgentDefaults::default(),
            agent_order: Vec::new(),
            agent_settings_offered: false,
            agent_settings_request: false,
            reveal_paths_offered: false,
            reveal_path_request: None,
            favorite_models: Vec::new(),
            favorite_models_request: None,
            openai_models: Vec::new(),
            ollama_models: Vec::new(),
            agent_status: HashMap::new(),
            agent_status_asked: None,
            agent_models: HashMap::new(),
            agent_models_asked: Default::default(),
            setup_agent: None,
            working_folder_request: None,
            response_hover: None,
            response_message_hover: None,
            tool_activity_hover: None,
            tool_workspace_offset: 0,
            tool_workspace_drag: None,
            copy_feedback: Tooltip::default(),
            link_card: link_card::LinkCard::default(),
            web_search_tooltip: Tooltip::default(),
            response_selection: None,
            response_menu: None,
            images: HashMap::new(),
            pending_model_prefetch: None,
            retry_link: None,
            ollama_search_link: None,
            stored_document: None,
            stored_open_link: None,
            staged_embedded_send: None,
            durable_observations: 0,
            destinations: Vec::new(),
            workspace_catalog_error: None,
            selected_destination: 0,
            destination_open: None,
            pending_link_chat: None,
            profile: None,
            wants_open_elsewhere: None,
            highlight: None,
            confirm: None,
            destructive_action: None,
            stop_armed_at: None,
            sent_inputs: Vec::new(),
            history_cursor: None,
            history_entries: None,
            history_draft: None,
            history_attachment_draft: None,
            history_chat: None,
            recall_restages: HashMap::new(),
            recall_retired: HashMap::new(),
            greeting: None,
            strips: attachments::Strips::default(),
            attachment_work,
            thumbnails: attachments::ThumbCache::default(),
            pending_user_tiles: Vec::new(),
            sent_message: None,
            pending_grants: None,
            grants_ready: None,
            titles_in_flight: false,
            clipboard_reading: false,
            viewer: None,
            viewer_opening: None,
            viewer_lift: None,
            card_tile_hits: Vec::new(),
            queues: queue::Queues::default(),
            queue_tooltip: Tooltip::default(),
            reply_end: None,
            steer_after_cancel: None,
            sending_queued: false,
            steering: None,
            steered: Vec::new(),
            reduced_motion: false,
            replies_finished: 0,
            arrivals_reported: 0,
            asked_reported: false,
            activity: 0,
            activity_reported: 0,
            wants_chat_step: None,
            restored_scroll: None,
            new_divider: None,
            dictation: dictation::ComposerDictation::default(),
            skills: skills::SkillList::default(),
            lang: koru_i18n::Lang::for_system(
                koru_window::system_locale::system_locale().as_deref(),
            ),
        }
    }

    /// Start the background API client on first use. A context surface
    /// constructed behind a closed pane performs no discovery until a host
    /// activates it — opening Add… does, so the Chats flyout can list recents
    /// without the pane being open.
    pub fn activate(&mut self) -> bool {
        if self.reader.is_some() || cfg!(any(test, feature = "no-client")) {
            return false;
        }
        self.reader = Reader::start(self.root.clone());
        if let Some(reader) = &self.reader {
            self.waiting = reader
                .send(Command::Boot(self.pending_link_chat, self.profile))
                .is_ok();
            true
        } else {
            self.status = Some("Koru AI service is not connected.".into());
            true
        }
    }

    /// Start a brand-new chat, suppressing the active document context for this
    /// chat so it begins empty (workspace context still applies).
    pub fn start_new_chat(&mut self) -> bool {
        self.suppress_next_document_context = true;
        self.fresh_chat_wanted = true;
        // Before the boot has answered there is no provider to create under:
        // the intent is kept, and the boot's snapshot creates the chat
        // instead of handing this tab the service's current one.
        if self.settings.is_none() {
            return true;
        }
        self.create_chat()
    }

    /// Open a durable chat link. Calling this before activation makes the
    /// first read target the link directly; calling it later queues a read.
    pub fn open_link(&mut self, chat: ChatId, block: Option<MessageBlockId>) -> bool {
        self.fresh_chat_wanted = false;
        self.pending_link_chat = Some(chat);
        self.highlight = block.map(|block| (block, Instant::now()));
        self.pending_question_anchor = None;
        self.scroll_to_pending_question = false;
        if self.reader.is_some() {
            self.scroll_loaded_chat_to_bottom = true;
            self.send_command(Command::Read(chat))
        } else {
            true
        }
    }

    pub fn set_context_document(&mut self, document: Option<(DocumentId, String)>) -> bool {
        let document = if self.suppress_next_document_context {
            self.suppress_next_document_context = false;
            None
        } else {
            document
        };
        let new_id = document.as_ref().map(|(id, _)| *id);
        let current_id = self.context_document.as_ref().map(|(id, _)| *id);
        if new_id == current_id {
            // Title may have changed; update it without touching dismissal state.
            if document.as_ref().map(|(_, title)| title)
                != self.context_document.as_ref().map(|(_, title)| title)
            {
                self.context_document = document;
                return true;
            }
            return false;
        }
        if new_id.is_some() && new_id == self.dismissed_context_document {
            // The user dismissed this document while it was active; keep it dismissed.
            return false;
        }
        self.dismissed_context_document = None;
        self.context_document = document;
        true
    }

    fn remove_context_document(&mut self) {
        if let Some((id, _)) = self.context_document.take() {
            self.dismissed_context_document = Some(id);
        }
    }

    pub fn clear_context_document_dismissal(&mut self) {
        self.dismissed_context_document = None;
    }

    /// Tell an embedded surface which workspace its host is currently showing.
    /// The workspace is presented first and becomes the initial grant for each
    /// chat; users may still remove it afterwards.
    pub fn set_context_workspace(&mut self, workspace: Option<WorkspaceId>) -> bool {
        if self.context_workspace == workspace {
            return false;
        }
        self.context_workspace = workspace;
        self.prioritize_context_workspace();
        true
    }

    /// The durable title of the loaded chat, when there is one. Used by hosts
    /// that show the chat as a tab and want the label to follow renames.
    pub fn current_chat_title(&self) -> Option<&str> {
        self.transcript.as_ref().map(|t| t.chat.title.as_str())
    }

    /// The chat this surface is seated on: the loaded transcript, or the
    /// link it is still opening. A host that opens a tab onto a recent chat
    /// uses this to focus that tab before the read has landed, so a second
    /// pick of the same row does not mint another.
    pub fn current_chat_id(&self) -> Option<ChatId> {
        // A chat not made yet has no id the service, or a host, can use.
        self.shown_chat_id().filter(|_| !self.chat_unmade())
    }

    /// Whether the chat on screen is made: the service holds it, because its
    /// first message was sent or it was opened from the list. `false` with
    /// nothing on screen yet, and for a new chat not sent — which a host
    /// shows as a new chat rather than by its title and agent (D22).
    pub fn current_chat_made(&self) -> bool {
        self.transcript.is_some() && !self.chat_unmade()
    }

    /// The chat's title once it has one of its own: `None` while it still
    /// carries the placeholder a chat is made with, before its automatic
    /// title lands — a stored English word a host should not show as a name.
    pub fn current_chat_own_title(&self) -> Option<&str> {
        self.current_chat_title()
            .filter(|title| *title != DEFAULT_CHAT_TITLE)
    }

    /// Who answers the chat on screen, by provider id — `agent-claude`,
    /// `openai`, `koru`. [`provider_mark`] is the face that goes with it.
    pub fn current_chat_provider(&self) -> Option<&str> {
        self.transcript
            .as_ref()
            .map(|t| t.chat.current_model.provider.as_str())
    }

    /// The chat on screen, a stand-in not made yet included, or the one a
    /// link is opening.
    fn shown_chat_id(&self) -> Option<ChatId> {
        self.transcript
            .as_ref()
            .map(|t| t.chat.id)
            .or(self.pending_link_chat)
    }

    /// Recent chats, most recent first, for a host menu. Empty until the
    /// surface has booted and listed them.
    pub fn recent_chats(&self, limit: usize) -> Vec<(ChatId, String)> {
        self.chats
            .iter()
            .take(limit)
            .map(|chat| (chat.id, chat.title.clone()))
            .collect()
    }

    /// Apply queued service updates without the visible-surface clocks.
    /// A host that activated this surface only to list chats uses this while
    /// the pane is closed, so Boot can land without a draw.
    pub fn drain_updates(&mut self) -> bool {
        let mut changed = false;
        while let Some(update) = self.reader.as_ref().and_then(Reader::try_update) {
            changed |= self.apply_update(update);
        }
        changed
    }

    /// When the next poll for a pending Boot (or other command) is due.
    pub fn listing_deadline(&self) -> Option<Instant> {
        self.waiting.then(|| {
            Instant::now()
                .checked_add(POLL_INTERVAL)
                .unwrap_or_else(Instant::now)
        })
    }

    /// A folder pick the host performs for an agent chat: the request names
    /// the chat and where the picker starts. Cancelling answers nothing.
    pub fn take_working_folder_request(&mut self) -> Option<WorkingFolderRequest> {
        self.working_folder_request.take()
    }

    /// The host's answer to [`take_working_folder_request`](Self::take_working_folder_request).
    pub fn working_folder_chosen(&mut self, chat: ChatId, folder: std::path::PathBuf) -> bool {
        if self.transcript.as_ref().map(|t| t.chat.id) != Some(chat) {
            return false;
        }
        self.send_command(Command::UpdateFolder(
            chat,
            koru_ai_wire::UpdateChatFolderRequest {
                folder: Some(folder.to_string_lossy().into_owned()),
            },
        ))
    }

    pub fn take_openai_credential_request(&mut self) -> Option<OpenAiCredentialRequest> {
        self.credential_request.take()
    }

    /// Complete the host-owned vault mutation. Only after success do we ask
    /// the service to save non-secret model settings and detect vault presence.
    pub fn openai_credential_saved(&mut self, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => {
                let Some((model, reasoning)) = self.pending_openai_settings.take() else {
                    return false;
                };
                self.send_command(Command::UpdateOpenAi(UpdateOpenAiSettingsRequest {
                    model,
                    reasoning,
                }))
            }
            Err(message) => {
                self.pending_openai_settings = None;
                self.status = Some(message);
                true
            }
        }
    }

    pub fn set_viewport(&mut self, area: Rect) -> bool {
        if self.viewport == area {
            return false;
        }
        if (self.viewport.w - area.w).abs() > f32::EPSILON {
            self.messages_dirty = true;
        }
        self.viewport = area;
        self.viewer_resized();
        true
    }

    pub fn invalidate(&mut self) {
        let workspace_search = self.workspace_search.value();
        let openai_key = self.openai_key.value();
        let openai_model = self.openai_model.value();
        let ollama_endpoint = self.ollama_endpoint.value();
        let ollama_model = self.ollama_model.value();
        let system_prompt = self.system_prompt.value();
        self.engine = TextEngine::new();
        self.composer.invalidate();
        self.workspace_search =
            model_picker::SharedInput::with_text(&mut self.engine, role::ui(), &workspace_search);
        self.openai_key = TextInput::with_text(&mut self.engine, role::row(), &openai_key);
        self.openai_model = TextInput::with_text(&mut self.engine, role::row(), &openai_model);
        self.ollama_endpoint =
            TextInput::with_text(&mut self.engine, role::row(), &ollama_endpoint);
        self.ollama_model = TextInput::with_text(&mut self.engine, role::row(), &ollama_model);
        self.system_prompt = TextArea::new(&mut self.engine, role::ui());
        self.system_prompt
            .set_text(&mut self.engine, &system_prompt);
        // Every kept layout was shaped in the old engine's fonts: a message
        // or a call's detail reused as it is would keep them after the App
        // font changed. Laid out again, once each.
        self.message_layouts.clear();
        self.call_details.clear();
        self.messages_dirty = true;
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        let poll = self.waiting.then(|| {
            Instant::now()
                .checked_add(POLL_INTERVAL)
                .unwrap_or_else(Instant::now)
        });
        let highlight = self
            .highlight
            .and_then(|(_, since)| since.checked_add(HIGHLIGHT_FOR));
        let image = self
            .images
            .values()
            .any(|image| image.pending.is_some())
            .then(|| Instant::now() + Duration::from_millis(100));
        // The stop hint goes away when its window closes.
        let stop_hint = self
            .stop_armed_at
            .and_then(|at| at.checked_add(STOP_ESCAPE_WINDOW));
        [
            poll,
            highlight,
            image,
            stop_hint,
            self.copy_feedback.maintenance_deadline(),
            self.link_card.deadline(),
            self.web_search_tooltip.maintenance_deadline(),
            self.picker_tooltip.maintenance_deadline(),
            self.queue_tooltip.maintenance_deadline(),
            self.skill_tooltip().maintenance_deadline(),
            self.composer.maintenance_deadline(),
            self.dictation_deadline(),
            self.attachment_deadline(),
            self.work_tick_deadline(),
            self.modes_deadline(),
            self.viewer
                .as_ref()
                .and_then(|open| open.viewer.maintenance_deadline()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn maintain(&mut self, _now: Instant) -> bool {
        // A staged embedded send needs another host frame so the Store has an
        // opportunity to publish and acknowledge the edit that preceded it.
        let mut changed = self.staged_embedded_send.is_some();
        changed |= self.drain_updates();
        self.follow_agent_checks(_now);
        self.follow_agent_modes(_now);
        if self
            .highlight
            .is_some_and(|(_, since)| _now.duration_since(since) >= HIGHLIGHT_FOR)
        {
            self.highlight = None;
            changed = true;
        }
        if self.stop_armed_at.is_some() && !self.stop_armed(_now) {
            self.stop_armed_at = None;
            changed = true;
        }
        changed |= self.maintain_images();
        // A running reply's *Working for 6s* moves on once a second, and
        // its light sweeps at the status frame (D83).
        if self
            .work_tick_deadline()
            .is_some_and(|deadline| _now >= deadline)
        {
            self.work_ticked_at = Some(_now);
            changed = true;
        }
        changed |= self.copy_feedback.maintain(_now);
        changed |= self.model_chevron.advance(_now);
        changed |= self.microphone_chevron.advance(_now);
        changed |= self.send_chevron.advance(_now);
        changed |= self.send_chevron_reveal.advance(_now);
        changed |= self.link_card.maintain(_now);
        changed |= self.web_search_tooltip.maintain(_now);
        changed |= self.picker_tooltip.maintain(_now);
        changed |= self.queue_tooltip.maintain(_now);
        changed |= self.maintain_skill_tooltip(_now);
        // The `@` picker's search answers, delivered on this tick.
        self.composer
            .set_context(self.context_workspace, self.context_document.clone());
        changed |= self.composer.maintain();
        changed |= self.maintain_dictation(_now);
        changed |= self.maintain_attachments();
        if let Some(open) = &mut self.viewer {
            changed |= open.viewer.maintain(_now);
        }
        changed
    }

    /// Let an embedding host release a staged turn only after Store reports no
    /// unacknowledged work. Two observations are required so a local edit that
    /// was still unsent during the press cannot look durable merely because it
    /// had not entered the Store session yet.
    pub fn submit_embedded_when_durable(&mut self, durable: bool) -> bool {
        if self.staged_embedded_send.is_none() {
            return false;
        }
        if !durable {
            self.durable_observations = 0;
            return false;
        }
        self.durable_observations = self.durable_observations.saturating_add(1);
        if self.durable_observations < 2 {
            return false;
        }
        self.durable_observations = 0;
        let request = self
            .staged_embedded_send
            .take()
            .expect("a staged send was checked above");
        if !self.queue_command(Command::Send(request)) {
            self.give_back_sent(None);
        }
        true
    }

    pub fn animating(&self) -> bool {
        self.copy_feedback.animating(Instant::now())
            || self.model_chevron.animating()
            || self.microphone_chevron.animating()
            || self.send_chevron.animating()
            || self.send_chevron_reveal.animating()
            || self.web_search_tooltip.animating(Instant::now())
            || self.picker_tooltip.animating(Instant::now())
            || self.queue_tooltip.animating(Instant::now())
            || self.skill_tooltip().animating(Instant::now())
            || self.dictation_animating(Instant::now())
            || self.pulse.as_ref().is_some_and(PulsingDots::animating)
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        if let Some(cursor) = self.viewer_cursor() {
            return cursor;
        }
        if let Some((_, choice)) = &self.computer_file_choice {
            return choice.cursor();
        }
        if self.confirm.is_some() || self.response_menu.is_some() || self.plus_menu.is_some() {
            return CursorIcon::Default;
        }
        let p = self.pointer;
        // The open model picker lies over the conversation, whose text
        // would otherwise give its I-beam to the rows (D44).
        if let Some(cursor) = self.picker_cursor(p.0, p.1) {
            return cursor;
        }
        // ↓ floats over the transcript's text: the arrow, as every press
        // target has (ADR-215).
        if self
            .jump_button()
            .is_some_and(|button| button.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        if self
            .composer
            .picker_rect()
            .is_some_and(|panel| panel.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        // The `$` list floats over the transcript's text too: the arrow on
        // its rows, never a hand (D121, ADR-215).
        if self
            .skill_panel()
            .is_some_and(|panel| panel.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        // *Add to chat* floats over the reply's text: the arrow (ADR-215).
        if self
            .add_pill
            .rect()
            .is_some_and(|rect| rect.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        // So does a link's card (D95).
        if self
            .link_card
            .card_rect(Instant::now())
            .is_some_and(|rect| rect.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        if self.conversation_scroll_drag.is_some()
            || self
                .conversation_scrollbar()
                .is_some_and(|scrollbar| scrollbar.in_gutter(p.0, p.1))
        {
            return CursorIcon::NsResize;
        }
        if self.picker.scroll_drag.is_some() || self.tool_workspace_drag.is_some() {
            return CursorIcon::NsResize;
        }
        if self.retry_link.is_some_and(|rect| rect.contains(p.0, p.1))
            || self
                .ollama_search_link
                .is_some_and(|rect| rect.contains(p.0, p.1))
            || self
                .stored_open_rect()
                .is_some_and(|rect| rect.contains(p.0, p.1))
        {
            return CursorIcon::Default;
        }
        if self.control_hover_at(p.0, p.1).is_some() || self.response_hover.is_some() {
            return CursorIcon::Default;
        }
        if self.tool_activity_at(p.0, p.1).is_some() || self.work_hit_at(p.0, p.1).is_some() {
            return CursorIcon::Default;
        }
        if let Some(hit) = self.response_text_hit(p.0, p.1) {
            return if hit.link.is_some() {
                CursorIcon::Default
            } else {
                CursorIcon::Text
            };
        }
        if self
            .tool_access_search_rect()
            .is_some_and(|rect| rect.contains(p.0, p.1))
            || (!self.showing_setup() && self.composer_text_rect().contains(p.0, p.1))
            || (self.showing_setup() && self.setup_text_field_at(p.0, p.1))
            || self
                .system_prompt_rect()
                .is_some_and(|r| r.contains(p.0, p.1))
        {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        // Any key lets a `$` row's description go at once (D121).
        let tooltip_gone = self.skill_tooltip_clear();
        self.route_key(key, text, mods) || tooltip_gone
    }

    fn route_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        // The viewer is modal: every key is its own while it is up.
        if let Some(handled) = self.viewer_key(key, text, mods) {
            return handled;
        }
        // A recording is stopped by its host (D103); a key never reaches the
        // composer mid-composition, and Escape never arms a reply's stop.
        if let Some(handled) = self.dictation_key(key) {
            return handled;
        }
        // The open microphone panel takes Up, Down, Enter and Escape, as
        // the approval-mode list does (D103).
        if let Some(handled) = self.microphone_key(key, mods) {
            return handled;
        }
        // A link's card (D95) goes on any key; Escape is spent on it.
        let card_up = self.link_card.visible(Instant::now());
        self.link_card.hide();
        if card_up && matches!(key, Key::Named(NamedKey::Escape)) {
            return true;
        }
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.key(key);
            return self.finish_confirmation(outcome);
        }
        if let Some(handled) = self.computer_choice_key(key) {
            return handled;
        }
        if let Some(handled) = self.plus_menu_key(key) {
            return handled;
        }
        if let Some(handled) = self.quote_key(key, text, mods) {
            return handled;
        }
        // The open model picker takes the keyboard: its search, rail, list
        // and footer (§9.5).
        if let Some(handled) = self.picker_key(key, text, mods) {
            return handled;
        }
        if self.tool_access_open && self.focus == Focus::WorkspaceSearch {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.tool_access_open = false;
                self.workspace_search.set_text(&mut self.engine, "");
                self.focus = Focus::Composer;
                return true;
            }
            let before = self.workspace_search.raw_text();
            let handled =
                self.workspace_search.key(&mut self.engine, key, text, mods) == Typed::Handled;
            if self.workspace_search.raw_text() != before {
                self.tool_workspace_offset = 0;
            }
            return handled;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) && self.setup_open {
            if self.transcript.is_some() {
                self.setup_open = false;
                self.focus = Focus::Composer;
                return true;
            }
        }
        if self.showing_setup() {
            return self.setup_key(key, text, mods);
        }
        if koru_keys::is_chord(mods)
            && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case("c"))
        {
            return self.copy_response_selection();
        }
        if mods.shift_key()
            && !koru_keys::has_primary(mods)
            && !mods.alt_key()
            && matches!(
                key,
                Key::Named(
                    NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                )
            )
            && self.move_response_selection(key)
        {
            self.show_add_pill();
            return true;
        }
        if Self::is_question_navigation_chord(key, mods) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] AI chat claimed=question-navigation direction={}",
                if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                    "down"
                } else {
                    "up"
                }
            ));
            return self.navigate_questions(matches!(key, Key::Named(NamedKey::ArrowDown)));
        }
        // **Ctrl+End jumps to the newest message** (D77) — unless there is
        // text in the composer for it to move the caret through.
        if koru_keys::registry::get(koru_keys::ShortcutId::JumpToLatest)
            .chord
            .matches(key, text, mods)
            && self.jump_key_is_ours()
        {
            return self.jump_to_latest();
        }
        // **Ctrl+Shift+↑/↓ step through the Chats list** (D87) — on the
        // same terms: in a message with text the chord stays the editor's.
        if let Some(step) = Self::chat_step_chord(key, text, mods)
            && self.jump_key_is_ours()
        {
            return self.step_chat(step);
        }
        if let Focus::Work(message) = self.focus
            && let Some(handled) = self.work_key(message, key, mods)
        {
            return handled;
        }
        if let Focus::Questions(tool) = self.focus
            && let Some(handled) = self.questions_key(tool, key, text, mods)
        {
            return handled;
        }
        // The split button is an independent keyboard stop. Its menu remains
        // reachable with an empty draft, when the send action is disabled.
        if let Some(handled) = self.approval_modes_key(key, mods) {
            return handled;
        }
        if matches!(self.focus, Focus::SendArrow | Focus::ModeChevron) {
            if self.approval_mode_button().is_none() {
                self.focus = Focus::Composer;
            } else if mods.is_empty() {
                match key {
                    Key::Named(NamedKey::Tab) => {
                        self.focus = if self.focus == Focus::SendArrow {
                            Focus::ModeChevron
                        } else {
                            Focus::Composer
                        };
                        return true;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if self.focus == Focus::SendArrow {
                            return self.activate_send_button();
                        }
                        self.approval_mode_open = true;
                        self.approval_mode_focus = None;
                        return true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.focus = Focus::Composer;
                        return true;
                    }
                    _ => self.focus = Focus::Composer,
                }
            }
        }
        // **A modifier pressed on its own is not typing** (D90's Ctrl+L
        // bug): the shell hands this surface the Ctrl of `Ctrl+L` as a key
        // of its own, and taking it as the keyboard arriving in the composer
        // moved the keyboard off the transcript a moment before the `L`. It
        // leaves the keyboard where it is.
        if matches!(
            key,
            Key::Named(
                NamedKey::Control
                    | NamedKey::Shift
                    | NamedKey::Alt
                    | NamedKey::AltGraph
                    | NamedKey::Super
                    | NamedKey::Meta
                    | NamedKey::Hyper
            )
        ) {
            return false;
        }
        if self.focus == Focus::Composer
            && mods.is_empty()
            && matches!(key, Key::Named(NamedKey::Tab))
            && self.approval_presentation().is_some()
            && self.send_mode() != queue::SendMode::Stop
            && !self.skill_list_open()
            && !self.composer.picker_open()
        {
            self.focus = Focus::SendArrow;
            return true;
        }
        if self.focus != Focus::Composer {
            self.focus = Focus::Composer;
        }
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        self.composer
            .set_context(self.context_workspace, self.context_document.clone());
        // **The `$` list's keys come first** (D121): while it is open, Up,
        // Down, Enter, Tab and Escape choose or dismiss a skill; Enter sends
        // only once it has closed.
        if let Some(handled) = self.skill_key(key, mods) {
            return handled;
        }
        // **The `@` picker's keys come first.** While it is open, Up, Down,
        // Enter, Tab and Escape choose or dismiss a document; Enter sends only
        // once it has closed.
        if self.composer.picker_open() {
            return self.composer_key(key, text, mods);
        }
        // **Shift+Tab steps the approval mode** (koru-ai D7) — after the
        // composer: an outdent wins wherever there is one to make.
        if koru_keys::registry::get(koru_keys::ShortcutId::CycleAgentMode)
            .chord
            .matches(key, text, mods)
        {
            return self.composer_key(key, text, mods) || self.cycle_approval_mode();
        }
        // **Escape while walking gives the draft back**, as Down past the
        // newest does — before it arms a stop.
        if matches!(key, Key::Named(NamedKey::Escape))
            && mods.is_empty()
            && self.history_cursor.is_some()
        {
            return self.leave_input_walk();
        }
        if matches!(key, Key::Named(NamedKey::Escape))
            && let Some(attempt) = self.attempt
        {
            return self.escape_during_turn(attempt, Instant::now());
        }
        // **Up and Down walk the messages this chat was sent**, from the
        // composer's first and last lines; inside a longer message they move
        // the caret as they always did. Alt and Shift with them were claimed
        // above, by question navigation and the response selection.
        if mods.is_empty()
            && matches!(key, Key::Named(NamedKey::ArrowUp))
            && self.composer.caret_on_first_line()
            && self.recall_input(true)
        {
            return true;
        }
        if mods.is_empty()
            && matches!(key, Key::Named(NamedKey::ArrowDown))
            && self.composer.caret_on_last_line()
            && self.recall_input(false)
        {
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Enter)) {
            // The editor's key map reads Shift+Enter as *split above* and
            // Ctrl+Enter as *toggle done*; in a message both are claimed here.
            if mods.shift_key() && !koru_keys::has_primary(mods) && !mods.alt_key() {
                return self.composer.insert_break();
            }
            if !koru_keys::is_chord(mods) && !mods.alt_key() {
                return self.send_composer();
            }
        }
        if mods.control_key()
            && !mods.shift_key()
            && !mods.alt_key()
            && koru_keys::letter(key, text) == Some('j')
        {
            return self.composer.insert_break();
        }
        self.composer_key(key, text, mods)
    }

    /// A key for the composer. A paste that found no text is answered here,
    /// at once, whichever path the key took: its picture is read off this
    /// thread (D9), and nothing is left armed for a later key.
    fn composer_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let handled = self.composer.key(key, text, mods);
        // A `$` typed, its query grown, or the caret moved out of it (D121).
        let handled = self.sync_skill_list() || handled;
        if self.composer.take_wants_paste_attachment() {
            return self.begin_attachment_paste() || handled;
        }
        handled
    }

    /// This chord belongs to a visible chat surface even when another part of
    /// the containing workspace most recently held keyboard focus.
    pub fn is_question_navigation_chord(key: &Key, mods: ModifiersState) -> bool {
        mods.alt_key()
            && !mods.control_key()
            && !mods.super_key()
            && !mods.shift_key()
            && matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown))
    }

    /// **Escape twice stops the work in progress.** The first arms the stop
    /// and says so above the composer; a second inside
    /// [`STOP_ESCAPE_WINDOW`] cancels the attempt exactly as the stop button
    /// does. One press alone never stops a turn — Escape is also how a
    /// person leaves whatever they were doing. An open workspace or mode
    /// popover takes the press first, so Escape closes it, not the turn.
    fn escape_during_turn(&mut self, attempt: InferenceAttemptId, now: Instant) -> bool {
        if self.tool_mode_open
            || self.tool_access_open
            || self.approval_mode_open
            || self.destination_open.is_some()
        {
            self.tool_mode_open = false;
            self.approval_mode_open = false;
            self.tool_access_open = false;
            self.destination_open = None;
            return true;
        }
        if self.stop_armed(now) {
            self.stop_armed_at = None;
            return self.send_command(Command::Cancel(attempt));
        }
        self.stop_armed_at = Some(now);
        true
    }

    /// Whether a first Escape is still waiting for its second at `now`.
    fn stop_armed(&self, now: Instant) -> bool {
        self.attempt.is_some()
            && self
                .stop_armed_at
                .is_some_and(|at| now.saturating_duration_since(at) < STOP_ESCAPE_WINDOW)
    }

    /// The line above the composer: the stop hint while it waits, else the
    /// status.
    fn footer_line(&self) -> Option<&str> {
        if self.stop_armed(Instant::now()) {
            return Some(koru_i18n::t(
                self.lang,
                koru_i18n::Key::AiPressEscAgainToStop,
            ));
        }
        self.status.as_deref()
    }

    /// The document *Open* opens (D78): the one just stored, while the
    /// footer still says so. The host opens it as a tab.
    fn stored_open_document(&self) -> Option<DocumentId> {
        let (line, document) = self.stored_document.as_ref()?;
        (self.footer_line() == Some(line.as_str())).then_some(*document)
    }

    /// *Open*'s rectangle, while it shows.
    fn stored_open_rect(&self) -> Option<Rect> {
        self.stored_open_document()?;
        self.stored_open_link
    }

    /// **The footer line, laid out** at `width`: its words, wrapped, and —
    /// after a stored reply (D78) — *Open*, with where it sits after the
    /// last line's end. The words leave room for it at their side.
    fn footer_layouts(
        &mut self,
        width: f32,
    ) -> Option<(Layout<Brush>, Option<(Layout<Brush>, f32, f32)>)> {
        let line = self.footer_line()?.to_owned();
        let open = self.stored_open_document().map(|_| {
            self.engine.simple_layout(
                koru_i18n::t(self.lang, koru_i18n::Key::AiOpenStoredDocument),
                role::meta(),
            )
        });
        let room = open
            .as_ref()
            .map_or(0.0, |open| open.width() + STORED_OPEN_GAP);
        let words = self
            .engine
            .simple_layout_wrapped(&line, role::meta(), (width - room).max(1.0));
        let open = open.map(|open| {
            let (end, top) = words.lines().last().map_or((0.0, 0.0), |last| {
                let metrics = last.metrics();
                (
                    metrics.advance - metrics.trailing_whitespace,
                    metrics.block_min_coord,
                )
            });
            (open, end + STORED_OPEN_GAP, top)
        });
        Some((words, open))
    }

    /// *Open* pressed (D78): the stored document opens as a tab, the way a
    /// document opened from the Files list does — the host's own
    /// open-document request.
    fn open_stored_document(&mut self) -> bool {
        let Some(document) = self.stored_open_document() else {
            return false;
        };
        self.open_document_request = Some(koru_link::KoruLink::Document {
            document: document.as_uuid(),
            node: None,
        });
        true
    }

    /// What Up walks back through, oldest first: each user message by its
    /// identity, then local sends the transcript has not returned yet.
    fn input_history(&self) -> Vec<HistoryInput> {
        let Some(transcript) = &self.transcript else {
            return Vec::new();
        };
        let mut asked: Vec<_> = transcript
            .messages
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .collect();
        asked.sort_by_key(|message| message.ordinal);
        let mut entries: Vec<HistoryInput> = asked
            .iter()
            .map(|message| HistoryInput {
                markdown: message.markdown.clone(),
                attachments: transcript
                    .attachments
                    .iter()
                    .filter(|attachment| attachment.message_id == Some(message.id))
                    .cloned()
                    .collect(),
            })
            .collect();
        for sent in &self.sent_inputs {
            if sent.chat == transcript.chat.id
                && sent
                    .message_id
                    .is_none_or(|id| !asked.iter().any(|message| message.id == id))
            {
                entries.push(HistoryInput {
                    markdown: sent.markdown.clone(),
                    attachments: sent.attachments.clone(),
                });
            }
        }
        entries
    }

    /// Step through [`ChatSurface::input_history`]: `older` is Up. Leaving the
    /// message being typed sets it aside whole, chips and all, and Down past
    /// the newest gives it back. A recalled message keeps its chips too.
    /// `false` when there is nowhere to go, so the key falls through to the
    /// editor.
    ///
    /// **The caret lands where the next step starts** (koru-ai D100): at
    /// the start of a message Up brought back, on its first line, and at
    /// the end of one Down brought back, on its last. It used to land at
    /// the end either way, so a message long enough to wrap had Up climb
    /// its lines before going on, and the walk read as stopping at the
    /// newest message.
    fn recall_input(&mut self, older: bool) -> bool {
        let entries = self
            .history_entries
            .clone()
            .unwrap_or_else(|| self.input_history());
        let next = match (self.history_cursor, older) {
            (None, false) => return false,
            (None, true) if entries.is_empty() => return false,
            (None, true) => Some(entries.len() - 1),
            // At the oldest already: the press is spent, the text stays.
            (Some(0), true) if !entries.is_empty() => return true,
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) => Some(index + 1),
        }
        .filter(|index| *index < entries.len());
        match next {
            Some(index) => {
                if self.history_cursor.is_none() {
                    self.history_draft = Some(self.composer.stash());
                    if let Some(chat) = self.current_chat_id() {
                        self.history_chat = Some(chat);
                        self.history_attachment_draft = Some(self.strips.take(chat));
                    }
                    self.history_entries = Some(entries.clone());
                }
                self.history_cursor = Some(index);
                if let Some(chat) = self.current_chat_id() {
                    self.replace_recalled_attachments(chat, &entries[index].attachments);
                }
                // The chips come back as chips, named as the conversation
                // names them: the caption sent, a title looked up since, or
                // the word an unknown one reads as.
                let titles = &self.document_titles;
                let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
                self.composer
                    .set_message(&entries[index].markdown, |document| {
                        titles
                            .get(&document)
                            .filter(|title| !title.trim().is_empty())
                            .cloned()
                            .unwrap_or_else(|| untitled.to_owned())
                    });
                if older {
                    self.composer.caret_to_start();
                }
            }
            None => {
                self.leave_input_walk();
            }
        }
        true
    }

    /// Stop walking and give back what was being typed before the walk
    /// began, chips, quotes and caret as they were: Down past the newest,
    /// and Escape while walking.
    fn leave_input_walk(&mut self) -> bool {
        if let Some(chat) = self.history_chat {
            self.replace_recalled_attachments(chat, &[]);
            if let Some(drafts) = self.history_attachment_draft.take() {
                for draft in drafts {
                    self.strips.push(chat, draft);
                }
            }
        }
        self.history_cursor = None;
        self.history_entries = None;
        self.history_chat = None;
        match self.history_draft.take() {
            Some(draft) => self.composer.unstash(draft),
            None => self.composer.clear(),
        }
        true
    }

    /// Forget where Up and Down stood — after a send, or on another chat.
    fn forget_input_walk(&mut self) {
        if let Some(chat) = self.history_chat {
            self.replace_recalled_attachments(chat, &[]);
        }
        if let (Some(chat), Some(drafts)) = (
            self.history_chat.take(),
            self.history_attachment_draft.take(),
        ) {
            for draft in drafts {
                self.strips.push(chat, draft);
            }
        }
        self.history_cursor = None;
        self.history_entries = None;
        self.history_draft = None;
    }

    /// Replace only the transient staged copies of the current history
    /// entry. A late result sees its id absent and deletes its copy.
    fn replace_recalled_attachments(
        &mut self,
        chat: ChatId,
        sources: &[koru_ai_core::ChatAttachment],
    ) {
        for draft in self.strips.take(chat) {
            if self.recall_restages.remove(&draft.id).is_some()
                && draft.state == attachments::DraftState::Storing
            {
                self.recall_retired.insert(draft.id, chat);
            }
            self.attachment_work.remove(chat, draft.id);
            self.thumbnails.remove(draft.id);
        }
        for source in sources {
            let id = koru_ai_core::ChatAttachmentId::new();
            self.recall_restages.insert(id, chat);
            self.strips
                .push(chat, attachments::Draft::restaged(source, id));
            let queued = if source.source == koru_ai_core::AttachmentSource::Path {
                if let Some(draft) = self.strips.draft_mut(chat, id) {
                    self.attachment_work.put(chat, draft);
                }
                true
            } else {
                self.queue_command(Command::Restage(chat, source.id, id))
            };
            if !queued {
                self.recall_restages.remove(&id);
                if let Some(draft) = self.strips.draft_mut(chat, id) {
                    draft.state = attachments::DraftState::Failed { retryable: true };
                }
            }
        }
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(handled) = self.viewer_mouse_down(x, y) {
            return handled;
        }
        if let Some(handled) = self.computer_choice_mouse_down(x, y) {
            return handled;
        }
        if let Some(handled) = self.plus_menu_mouse_down(x, y) {
            return handled;
        }
        if let Some(handled) = self.response_menu_mouse_down(x, y) {
            return handled;
        }
        if let Some(handled) = self.link_card_mouse_down(x, y) {
            return handled;
        }
        // *Add to chat* over the selection; any other press lets it go.
        if self.add_pill.rect().is_some_and(|rect| rect.contains(x, y)) {
            self.add_selection_to_chat();
            return true;
        }
        self.add_pill.hide();
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.mouse_down(
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
                x - self.viewport.x,
                y - self.viewport.y,
            );
            return self.finish_confirmation(outcome);
        }
        if self.showing_setup() {
            return self.setup_mouse_down(x, y);
        }
        // The composer and every control on it are hit where they are now:
        // centred under an empty chat's greeting, or at the bottom.
        self.composer_rect();
        // The `$` list floats over the conversation (D121): a press on it is
        // its own, and one anywhere else shuts it and goes on.
        if let Some(handled) = self.skill_mouse_down(x, y) {
            return handled;
        }
        // The model picker floats over the conversation and the composer:
        // a press on it is its own, and one outside it closes it.
        if let Some(handled) = self.picker_mouse_down(x, y) {
            return handled;
        }
        if let Some(handled) = self.strip_mouse_down(x, y) {
            return handled;
        }
        // The chevron beside the microphone and its panel (D103): a press
        // anywhere else shuts the panel and goes on.
        if let Some(handled) = self.microphone_mouse_down(x, y) {
            return handled;
        }
        // The microphone, or a press on a composer that is recording (D103).
        if let Some(handled) = self.dictation_mouse_down(x, y) {
            return handled;
        }
        // The `@` picker floats over the conversation, so a press on it is the
        // composer's before anything underneath can claim it.
        if self
            .composer
            .picker_rect()
            .is_some_and(|panel| panel.contains(x, y))
        {
            self.focus = Focus::Composer;
            return self.composer.mouse_down(x, y);
        }
        // A press anywhere but the composer answers an open picker the way
        // Escape does, and then does whatever it does.
        if !self.composer.contains(x, y) {
            self.composer.close_picker();
        }
        if self
            .stored_open_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            return self.open_stored_document();
        }
        if self
            .jump_button()
            .is_some_and(|button| button.contains(x, y))
        {
            return self.jump_to_latest();
        }
        if let Some(scrollbar) = self.conversation_scrollbar()
            && scrollbar.in_gutter(x, y)
        {
            self.follow_tail = false;
            if scrollbar.on_thumb(x, y) {
                // Preserve the exact grab point so the thumb does not jump
                // underneath the pointer when the drag begins.
                self.conversation_scroll_drag = Some(y - scrollbar.thumb_top);
            } else {
                // The invisible track is still an interaction target: page
                // toward the press, matching the document scrollbar.
                self.conversation_scroll += if y < scrollbar.thumb_top {
                    -self.conversation_hit_rect.h
                } else {
                    self.conversation_hit_rect.h
                };
                self.conversation_scroll =
                    self.conversation_scroll.clamp(0.0, scrollbar.max_scroll);
            }
            return true;
        }
        if self.destination_open.is_some() {
            if let Some(index) = self.destination_at(x, y) {
                self.selected_destination = index;
                let message = self.destination_open.expect("picker is open");
                return self.start_export(message);
            }
            self.destination_open = None;
            return true;
        }
        // A sent picture opens in the viewer (R3), a sent file in the
        // system's app (R4).
        if let Some(tile) = self.card_tile_at(x, y)
            && let Some(chat) = self.current_chat_id()
        {
            return self.open_tile(chat, &tile);
        }
        if let Some(handled) = self.approval_mode_mouse_down(x, y) {
            return handled;
        }
        if self
            .privacy_button()
            .is_some_and(|button| button.contains(x, y))
        {
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            return self.toggle_chat_privacy();
        }
        if self.tool_mode_open {
            if let Some(modes) = self.tool_mode_buttons()
                && let Some(policy) = self
                    .transcript
                    .as_ref()
                    .map(|transcript| transcript.tool_policy.clone())
            {
                if modes[0].contains(x, y) {
                    return self.update_tool_policy(ToolMode::Manual, policy.workspace_ids);
                }
                if modes[1].contains(x, y) {
                    return self.update_tool_policy(ToolMode::Auto, policy.workspace_ids);
                }
            }
        }
        if self.tool_access_open {
            // The list's scrollbar first: its gutter is over the rows' ends.
            if self.tool_access_press_scrollbar(x, y) {
                return true;
            }
            if self
                .tool_access_search_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                let rect = self
                    .tool_access_search_rect()
                    .expect("an open searchable workspace panel has a field");
                self.workspace_search.click(&mut self.engine, rect, x, y);
                self.focus = Focus::WorkspaceSearch;
                return true;
            }
            if let Some(workspace) = self.tool_access_workspace_at(x, y) {
                return self.toggle_tool_workspace(workspace);
            }
        }
        if self
            .tool_mode_button()
            .is_some_and(|button| button.contains(x, y))
        {
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = !self.tool_mode_open;
            return true;
        }
        if self
            .reasoning_toggle_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            self.reasoning_expanded = !self.reasoning_expanded;
            return true;
        }
        if self
            .web_search_button()
            .is_some_and(|button| button.contains(x, y))
        {
            self.web_search_armed = !self.web_search_armed;
            self.chat_model_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            self.web_search_tooltip.clear();
            return true;
        }
        if self.chat_model_button().contains(x, y) {
            if self.chat_model_open {
                self.tool_access_open = false;
                self.tool_mode_open = false;
                self.close_picker();
            } else {
                self.open_model_picker();
            }
            if self.chat_model_open
                && self.picker_rail().is_empty()
                && self.effort_control().is_none()
            {
                if let Some(provider) = self
                    .transcript
                    .as_ref()
                    .map(|transcript| transcript.chat.current_model.provider.clone())
                {
                    return self.send_command(Command::Validate(provider));
                }
            }
            return true;
        }
        if self.tool_mode_open || self.tool_access_open {
            self.tool_mode_open = false;
            self.tool_access_open = false;
            return true;
        }
        if let Some(hit) = self.queue_hit_at(x, y) {
            return self.queue_action(hit);
        }
        if self.send_button().contains(x, y) {
            return self.activate_send_button();
        }
        if let Some(hit) = self.work_hit_at(x, y) {
            if let work_view::WorkHit::Header(message) = hit {
                self.focus = Focus::Work(message);
                self.composer.set_focused(false);
            }
            return self.toggle_work(hit);
        }
        if let Some(handled) = self.question_mouse_down(x, y) {
            return handled;
        }
        if let Some((tool, part)) = self.tool_activity_at(x, y) {
            // A permission card is always open: only its choices act.
            return match part {
                part if part >= agents::CHOICE_PART_BASE => self.decide_agent_choice(tool, part),
                1 => self.send_command(Command::ApproveTool(tool)),
                2 => self.send_command(Command::DenyTool(tool)),
                _ => true,
            };
        }
        if let Some(action) = self.response_action_at(x, y) {
            return self.perform_response_action(action);
        }
        if let Some(url) = self.source_link_at(x, y) {
            open_web_link(&url);
            return true;
        }
        if let Some(url) = self.user_link_at(x, y) {
            return self.follow_link(&url);
        }
        if let Some(hit) = self.response_text_hit(x, y) {
            self.focus = Focus::None;
            let position = hit.position;
            self.response_selection = Some(ResponseSelection {
                anchor: position,
                focus: position,
                dragging: true,
                pressed_link: hit.link,
            });
            return true;
        }
        self.response_selection = None;
        if self.composer.contains(x, y) {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
            let handled = self.composer.mouse_down(x, y);
            // The caret may have landed in a `$` token, or left one (D121).
            self.sync_skill_list();
            return handled;
        }
        false
    }

    /// Follow a link the person clicked in the conversation: a Koru document
    /// goes to the host to open, a web address to the browser.
    fn follow_link(&mut self, url: &str) -> bool {
        match koru_link::parse(url) {
            Ok(link @ koru_link::KoruLink::Document { .. }) => {
                self.open_document_request = Some(link);
                true
            }
            _ => {
                open_web_link(url);
                true
            }
        }
    }

    /// **The link in the conversation under a point** (D95) — in a reply's
    /// text or one of its tables' cells, or a document link in the
    /// person's own message — with what it reads as and the line box it is
    /// drawn in, for its card. Every block is asked through the same
    /// [`BlockLayout::link_at`] that lights and follows it.
    fn conversation_link_at(&self, x: f32, y: f32) -> Option<link_card::ConversationLink> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for message in &self.message_layouts {
            let card = message_card(
                area,
                area.y - self.conversation_scroll + message.top,
                message_height(message),
                message,
            );
            // Asked on every move: a message the pointer is not on is not
            // walked.
            if !card.contains(x, y) {
                continue;
            }
            match message.role {
                ChatRole::Assistant => {
                    let mut block_y = card.y + PAD;
                    for block in &message.blocks {
                        let origin = (card.x + PAD, block_y);
                        if let Some(hit) = block.link_at(x - origin.0, y - origin.1) {
                            return Some(link_card::ConversationLink::from_hit(
                                &hit,
                                origin,
                                (x, y),
                            ));
                        }
                        block_y += block.height() + BLOCK_GAP;
                    }
                }
                ChatRole::User if !message.links.is_empty() => {
                    let origin = (card.x + PAD, card.y + PAD);
                    if let Some(hit) = koru_markdown::link_at(
                        &message.display,
                        &message.layout,
                        &message.links,
                        x - origin.0,
                        y - origin.1,
                    ) {
                        return Some(link_card::ConversationLink::from_hit(&hit, origin, (x, y)));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// A press on a link's card (D95): **copy** puts the address on the
    /// clipboard, **open** does what clicking the link does, and the rest
    /// of the card takes the press so nothing under it does. `None` when
    /// the press is not on the card.
    fn link_card_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        use koru_editor::draw::LinkCardHit;
        let Some((hit, url)) = self
            .link_card
            .hit(x, y, Instant::now())
            .map(|(hit, link)| (hit, link.url.clone()))
        else {
            // Any other press lets the card go.
            self.link_card.hide();
            return None;
        };
        match hit {
            LinkCardHit::Copy => {
                self.link_card.hide();
                // `koru_ui`'s own test guard does not reach this crate's
                // tests, which must not write the real clipboard.
                #[cfg(test)]
                tests::COPIED.with(|copied| copied.borrow_mut().push(url.clone()));
                #[cfg(not(test))]
                koru_ui::clipboard::put_plain(&url);
                self.show_copy_feedback(koru_i18n::t(self.lang, koru_i18n::Key::AiLinkCopied));
            }
            LinkCardHit::Action => {
                self.link_card.hide();
                self.follow_link(&url);
            }
            LinkCardHit::Body | LinkCardHit::None => {}
        }
        Some(true)
    }

    /// The buttons on a link's card while it is up, each with its name in
    /// the person's language — copy, then open (D95) — for a host that
    /// publishes the conversation to a screen reader.
    pub fn link_card_buttons(&self) -> Vec<(Rect, &'static str)> {
        self.link_card.buttons(self.lang, Instant::now())
    }

    /// The document link under a point in one of the person's own messages.
    fn user_link_at(&self, x: f32, y: f32) -> Option<String> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for message in &self.message_layouts {
            if message.role != ChatRole::User || message.links.is_empty() {
                continue;
            }
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            let origin = (card.x + PAD, card.y + PAD);
            let Some(index) = layout_cache::byte_at(&message.layout, x - origin.0, y - origin.1)
            else {
                continue;
            };
            if let Some(link) = message
                .links
                .iter()
                .find(|link| link.range.contains(&index))
            {
                return Some(link.url.clone());
            }
        }
        None
    }

    /// Close every transient picker, menu, or confirmation, returning whether
    /// anything changed. The host calls this when a press lands outside the AI
    /// surface so an open popover does not keep floating over the document.
    pub fn dismiss_popovers(&mut self) -> bool {
        let mut changed = false;
        if self.chat_model_open {
            self.close_picker();
            changed = true;
        }
        if self.tool_access_open {
            self.tool_access_open = false;
            changed = true;
        }
        if self.tool_mode_open {
            self.tool_mode_open = false;
            changed = true;
        }
        if self.model_options_open {
            self.model_options_open = false;
            changed = true;
        }
        changed |= self.close_microphones();
        if self.destination_open.take().is_some() {
            changed = true;
        }
        if self.response_menu.take().is_some() {
            self.response_menu_anchor = None;
            changed = true;
        }
        if self.plus_menu.take().is_some() {
            changed = true;
        }
        if self.skill_list_open() {
            changed |= self.dismiss_skill_list();
        }
        if self.computer_file_choice.take().is_some() {
            changed = true;
        }
        if self.confirm.take().is_some() {
            changed = true;
        }
        changed | self.composer.close_picker()
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some(changed) = self.viewer_mouse_move(x, y) {
            return changed;
        }
        if let Some((_, choice)) = &mut self.computer_file_choice {
            self.pointer = (x, y);
            return choice.mouse_move(
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
                x - self.viewport.x,
                y - self.viewport.y,
            );
        }
        let old = self.cursor_icon();
        let old_control_hover = self.control_hover_at(self.pointer.0, self.pointer.1);
        let old_pointer = self.pointer;
        self.pointer = (x, y);
        let search_tooltip_changed = self.web_search_tooltip.set_hover(
            self.web_search_button()
                .filter(|button| button.contains(x, y))
                .map(|button| {
                    TooltipContent::new(
                        21,
                        button,
                        "Search the web for the next replies in this chat",
                    )
                }),
            Instant::now(),
        ) | self.dictation_hover(old_pointer, x, y)
            | self.skill_tooltip_hover(x, y);
        if let Some(changed) = self.plus_menu_mouse_move(x, y) {
            return changed || old != self.cursor_icon();
        }
        if let Some(changed) = self.response_menu_mouse_move(x, y, old) {
            return changed;
        }
        if let Some(grab) = self.conversation_scroll_drag {
            if let Some(scrollbar) = self.conversation_scrollbar() {
                let next = scrollbar.scroll_for_pointer(y, grab);
                let changed = (next - self.conversation_scroll).abs() > f32::EPSILON;
                self.conversation_scroll = next;
                return changed || old != self.cursor_icon();
            }
            return true;
        }
        if let Some(changed) = self.picker_mouse_move(x, y) {
            return changed || old != self.cursor_icon();
        }
        if let Some(changed) = self.tool_access_drag(y) {
            return changed || old != self.cursor_icon();
        }
        if let Some(confirm) = &mut self.confirm {
            let changed = confirm.mouse_move(
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
                x - self.viewport.x,
                y - self.viewport.y,
            );
            return changed || old != self.cursor_icon();
        }
        let pill_changed = self.add_pill_mouse_move(x, y);
        let old_response_hover = self.response_hover;
        let old_message_hover = self.response_message_hover;
        let old_tool_hover = self.tool_activity_hover;
        let old_work_hover = self.work_hover;
        self.response_message_hover = if self.showing_setup() {
            None
        } else {
            self.response_message_hover_at(x, y, old_message_hover)
        };
        self.response_hover = if self.showing_setup() {
            None
        } else {
            self.response_action_hover_at(x, y)
        };
        self.tool_activity_hover = if self.showing_setup() {
            None
        } else {
            self.tool_activity_at(x, y)
        };
        self.work_hover = if self.showing_setup() {
            None
        } else {
            self.work_hit_at(x, y)
        };
        let dragged = if self.focus == Focus::WorkspaceSearch {
            self.tool_access_search_rect()
                .is_some_and(|rect| self.workspace_search.drag(&mut self.engine, rect, x))
        } else if self.showing_setup() {
            self.drag_setup_input(x)
        } else {
            self.composer_rect();
            self.composer.mouse_move(x, y)
        };
        let response_dragged = self.drag_response_selection(x, y);
        let under = (!self.showing_setup())
            .then(|| self.conversation_link_at(x, y))
            .flatten();
        let link_changed = self.link_card.track(under, (x, y), Instant::now());
        let queue_changed = !self.showing_setup() && self.queue_hover(x, y);
        let queue_hover_changed = !self.showing_setup()
            && self.queue_hit_at(old_pointer.0, old_pointer.1) != self.queue_hit_at(x, y);
        dragged
            || queue_changed
            || queue_hover_changed
            || pill_changed
            || response_dragged
            || link_changed
            || old_response_hover != self.response_hover
            || old_message_hover != self.response_message_hover
            || old_tool_hover != self.tool_activity_hover
            || old_work_hover != self.work_hover
            || old_control_hover != self.control_hover_at(x, y)
            || search_tooltip_changed
            || old != self.cursor_icon()
    }

    pub fn mouse_up(&mut self) -> bool {
        if let Some(handled) = self.viewer_mouse_up() {
            return handled;
        }
        self.conversation_scroll_drag = None;
        self.picker.scroll_drag = None;
        self.tool_workspace_drag = None;
        self.composer.mouse_up();
        self.workspace_search.release();
        self.openai_key.release();
        self.openai_model.release();
        self.ollama_endpoint.release();
        self.ollama_model.release();
        self.system_prompt.release();
        let selected = self
            .response_selection
            .as_ref()
            .is_some_and(|selection| selection.dragging);
        let pressed = self.response_selection.as_mut().and_then(|selection| {
            selection.dragging = false;
            (selection.anchor == selection.focus)
                .then(|| selection.pressed_link.take())
                .flatten()
        });
        // Followed only if let go over the link it was pressed on: in a
        // table two cells' links can share a position, so a press that
        // slid from one to the other does not open the first (D95).
        if let Some(url) = pressed {
            let (x, y) = self.pointer;
            if self.response_text_hit(x, y).and_then(|hit| hit.link) == Some(url.clone()) {
                self.follow_link(&url);
            }
        }
        // A drag through a reply let go: *Add to chat* over what it chose.
        if selected {
            self.show_add_pill();
        }
        true
    }

    pub fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        if let Some(handled) = self.viewer_double_click(x, y) {
            return handled;
        }
        if !self.showing_setup() && self.composer.contains(x, y) {
            return self.composer.mouse_double_click(x, y);
        }
        if let Some(mut hit) = self.response_text_hit(x, y) {
            if let Some((layout, origin)) =
                self.response_layout(hit.position.message, hit.position.block)
            {
                hit.selection = Selection::word_from_point(layout, x - origin.0, y - origin.1);
            }
            let anchor = ResponsePosition {
                byte: hit.selection.anchor().index(),
                affinity: hit.selection.anchor().affinity(),
                ..hit.position
            };
            let focus = ResponsePosition {
                byte: hit.selection.focus().index(),
                affinity: hit.selection.focus().affinity(),
                ..hit.position
            };
            self.response_selection = Some(ResponseSelection {
                anchor,
                focus,
                dragging: false,
                pressed_link: None,
            });
            self.show_add_pill();
            return true;
        }
        false
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) -> bool {
        if let Some(changed) = self.viewer_scroll(delta) {
            return changed;
        }
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        // A scroll lets *Add to chat* go; the selection stays. A link's
        // card goes too: what it hung from has moved.
        self.add_pill.hide();
        self.link_card.hide();
        // Over the composer's open picker, or over its own lines once they
        // overflow, the wheel is the composer's.
        let (px, py) = self.pointer;
        let chips = self.chip_band.band_rect(
            self.composer_hit_rect.x + COMPOSER_PAD_X,
            self.composer_hit_rect.y + COMPOSER_PAD_TOP,
            (self.composer_hit_rect.w - COMPOSER_PAD_X * 2.0).max(1.0),
        );
        if !self.showing_setup() && chips.contains(px, py) && self.chip_band.max_scroll() > 0.0 {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * (self.chip_band.shown_height / 3.0),
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            let next = (self.chip_scroll - dy).clamp(0.0, self.chip_band.max_scroll());
            let changed = (next - self.chip_scroll).abs() > f32::EPSILON;
            self.chip_scroll = next;
            self.chip_band.scroll = next;
            return changed;
        }
        if !self.showing_setup() && self.skill_scroll(px, py, delta) {
            return true;
        }
        if !self.showing_setup() && self.composer.scroll_by(px, py, delta) {
            return true;
        }
        if self.showing_setup() {
            if !self.model_options_open {
                return false;
            }
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            let previous = self.model_option_offset;
            let max = self
                .model_options()
                .len()
                .saturating_sub(MODEL_VISIBLE_ROWS);
            if dy > 0.0 {
                self.model_option_offset =
                    self.model_option_offset.saturating_sub(dy.ceil() as usize);
            } else if dy < 0.0 {
                self.model_option_offset =
                    (self.model_option_offset + (-dy).ceil() as usize).min(max);
            }
            return previous != self.model_option_offset;
        }
        if self.tool_access_open {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            let previous = self.tool_workspace_offset;
            let max = self
                .matching_workspace_indices()
                .len()
                .saturating_sub(self.tool_access_visible_rows());
            if dy > 0.0 {
                self.tool_workspace_offset = self.tool_workspace_offset.saturating_sub(1);
            } else if dy < 0.0 {
                self.tool_workspace_offset = (self.tool_workspace_offset + 1).min(max);
            }
            return previous != self.tool_workspace_offset;
        }
        if self.chat_model_open {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 30.0).clamp(-4.0, 4.0),
            };
            // Every row the list shows, whoever's: never only the chat's own
            // provider's models, which are none before a chat exists.
            return self.picker_scroll(dy);
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 38.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        } * CONVERSATION_SCROLL_SPEED;
        let max = (self.conversation_extent - self.conversation_rect().h).max(0.0);
        let next = (self.conversation_scroll - dy).clamp(0.0, max);
        let changed = (next - self.conversation_scroll).abs() > f32::EPSILON;
        // Scrolling up lets go of the newest message (D77).
        if next < self.conversation_scroll {
            self.follow_tail = false;
        }
        self.conversation_scroll = next;
        changed
    }

    /// **↓, back to the newest message** (D77): a round button centred over
    /// the transcript, just above the composer, while the transcript is
    /// scrolled up far enough that its last item ends more than
    /// [`JUMP_THRESHOLD`] of the viewport below the composer's top edge.
    /// `None` short of that, and where there is no transcript.
    fn jump_button(&self) -> Option<Rect> {
        if self.showing_setup() || self.transcript.is_none() {
            return None;
        }
        let area = self.conversation_hit_rect;
        // Where the last item ends on screen: the content's end, scrolled.
        let last_bottom = area.y - self.conversation_scroll + self.conversation_content_extent();
        if last_bottom - self.composer_hit_rect.y <= area.h * JUMP_THRESHOLD {
            return None;
        }
        Some(Rect::new(
            area.x + (area.w - JUMP_D) * 0.5,
            area.y + area.h - JUMP_D - JUMP_GAP,
            JUMP_D,
            JUMP_D,
        ))
    }

    /// **Jump to the newest message** (D77): ↓ pressed, or `Ctrl+End`. The
    /// transcript scrolls to its end — at once: it has no smooth scroll —
    /// and follows a reply that is still streaming from there on.
    pub fn jump_to_latest(&mut self) -> bool {
        let area = self.conversation_rect();
        self.question_navigation = None;
        self.conversation_scroll = (self.conversation_extent - area.h).max(0.0);
        self.follow_tail = true;
        true
    }

    /// Whether `Ctrl+End` is the chat's (D77): while the message is empty,
    /// or the keyboard is not in the composer. In a message with text it is
    /// the editor's caret to the end of the text, as ever.
    fn jump_key_is_ours(&self) -> bool {
        self.focus != Focus::Composer || self.composer.text().trim().is_empty()
    }

    /// Draw ↓ while it shows: the popover's surface and hairline, lifted,
    /// the arrow in it, lit under the pointer.
    fn draw_jump_button(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(button) = self.jump_button() else {
            return;
        };
        let name = koru_i18n::t(self.lang, koru_i18n::Key::AiJumpToLatest);
        let _record = koru_draw::inspect::scope(
            "Jump to latest",
            "part",
            root,
            scene::rect(button.x, button.y, button.w, button.h),
            |record| record.property("name", name, "koru_i18n::Key::AiJumpToLatest"),
        );
        let p = theme::p();
        scene::floating_panel(
            canvas,
            root,
            button.x,
            button.y,
            button.w,
            button.h,
            &scene::Panel::popover(button.w * 0.5),
        );
        if button.contains(self.pointer.0, self.pointer.1) {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(button.x, button.y, button.w, button.h, button.w * 0.5),
            );
        }
        // The catalog's ↑, turned about its centre: there is no ↓.
        let arrow = self.engine.icon_layout(icons::ARROW_UP, JUMP_ICON);
        let (cx, cy) = (
            f64::from(button.x + button.w * 0.5),
            f64::from(button.y + button.h * 0.5),
        );
        let turned = root * Affine::rotate_about(std::f64::consts::PI, (cx, cy));
        scene::draw_layout(
            canvas,
            turned,
            &arrow,
            button.x + (button.w - arrow.width()) * 0.5,
            button.y + (button.h - arrow.height()) * 0.5,
            Some(p.text),
        );
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.bg,
            &scene::rect(
                self.viewport.x,
                self.viewport.y,
                self.viewport.w,
                self.viewport.h,
            ),
        );
        if self.showing_setup() {
            self.draw_setup(canvas, root);
        } else {
            self.draw_chat(canvas, root);
        }
        if let Some(confirm) = &self.confirm {
            confirm.draw(
                canvas,
                root * Affine::translate((self.viewport.x as f64, self.viewport.y as f64)),
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
            );
        }
        if let Some((_, choice)) = &self.computer_file_choice {
            choice.draw(
                canvas,
                root * Affine::translate((self.viewport.x as f64, self.viewport.y as f64)),
                &mut self.engine,
                (self.viewport.w, self.viewport.h),
            );
        }
        let menu_viewport = self.menu_viewport();
        if let Some(menu) = &self.response_menu {
            menu.draw(canvas, root, &mut self.engine, menu_viewport, &[true, true]);
        }
        if let Some(menu) = &self.plus_menu {
            let enabled: Vec<_> = menu
                .items()
                .iter()
                .map(koru_ui::menu::MenuItem::selectable)
                .collect();
            menu.draw(canvas, root, &mut self.engine, menu_viewport, &enabled);
        }
        self.draw_unlifted_viewer(canvas, root);
    }

    /// Whether the provider setup is drawn instead of the chat. Only when
    /// opened on purpose, which nothing does for now ([`Self::open_setup`]):
    /// a surface with no ready provider shows the chat view, its chip asking
    /// for a model. It used to show the setup until a provider validated.
    fn showing_setup(&self) -> bool {
        self.setup_open
    }

    fn send_command(&mut self, command: Command) -> bool {
        self.queue_command(command);
        true
    }

    /// Hand a command to the reader, and say truthfully whether it was
    /// queued; a full or gone queue says so on the status line.
    fn queue_command(&mut self, command: Command) -> bool {
        let Some(command) = self.apply_to_unmade_chat(command) else {
            return true;
        };
        let Some(reader) = &self.reader else {
            self.status = Some("Koru AI service is not connected.".into());
            return false;
        };
        match reader.send(command) {
            Ok(()) => {
                self.waiting = true;
                self.status = None;
                return true;
            }
            Err(SendError::Full) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI dropped a command because the client queue is full"
                );
                self.status = Some("Koru AI is busy. Try again in a moment.".into());
            }
            Err(SendError::Disconnected) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI dropped a command because the client thread is disconnected"
                );
                self.status = Some("Koru AI service is not connected.".into());
            }
        }
        false
    }

    fn validate_models_after_settings(&mut self, selected: Option<AiProviderId>) {
        let openai = || AiProviderId::parse(AiProviderId::OPENAI).expect("built-in provider id");
        match selected {
            Some(provider) => {
                if provider.as_str() != AiProviderId::OPENAI && self.openai_credential_configured()
                {
                    self.pending_model_prefetch = Some(openai());
                }
                self.send_command(Command::Validate(provider));
            }
            None if self.openai_credential_configured() => {
                self.send_command(Command::Validate(openai()));
            }
            None => {}
        }
    }

    fn send_model_prefetch(&mut self, provider: AiProviderId) {
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.send(Command::Validate(provider)).is_ok() {
            self.waiting = true;
        }
    }

    fn open_confirmation(&mut self, action: DestructiveAction) {
        let (title, body, label) = match &action {
            DestructiveAction::ClearHistory => (
                "Clear all chat history?",
                "All AI conversations and search results will be removed. Anonymous usage and feedback totals remain.",
                "Clear history",
            ),
            DestructiveAction::ClearUsage => (
                "Clear usage data?",
                "All private token, timing, outcome, and feedback aggregates will be removed. Chat history is unchanged.",
                "Clear usage",
            ),
        };
        self.confirm = Some(Confirm::new(title, body, label));
        self.destructive_action = Some(action);
    }

    fn finish_confirmation(&mut self, outcome: ConfirmOutcome) -> bool {
        match outcome {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            ConfirmOutcome::Keep | ConfirmOutcome::Alternative => true,
            ConfirmOutcome::Cancel => {
                self.confirm = None;
                self.destructive_action = None;
                true
            }
            ConfirmOutcome::Confirm => {
                self.confirm = None;
                let Some(action) = self.destructive_action.take() else {
                    return true;
                };
                match action {
                    DestructiveAction::ClearHistory => {
                        self.setup_open = false;
                        self.send_command(Command::ClearHistory(self.profile))
                    }
                    DestructiveAction::ClearUsage => {
                        let chat = self
                            .transcript
                            .as_ref()
                            .map(|transcript| transcript.chat.id);
                        self.setup_open = false;
                        self.send_command(Command::ClearUsage(chat))
                    }
                }
            }
        }
    }

    fn apply_update(&mut self, update: Update) -> bool {
        if update.changes_chat_list() {
            self.chat_list_changed = true;
        }
        match update {
            Update::Boot {
                settings,
                chats,
                transcript,
                link_missing,
                destinations,
                destination_error,
            } => {
                // **The chat this tab was opened on is gone** — deleted since
                // the tab was saved (ADR-483-chattabrestore). The tab forgets
                // it and becomes a new chat, made once the provider
                // validates, rather than naming a chat nobody can open.
                if link_missing {
                    self.pending_link_chat = None;
                    self.fresh_chat_wanted = true;
                }
                // A tab opened for a new chat does not take the service's
                // current chat from the boot either — the last one used —
                // the same rule `HistoryChanged` keeps below. The chat it
                // asked for is created once the provider validates.
                let transcript = if self.fresh_chat_wanted {
                    None
                } else {
                    transcript
                };
                // **Another profile's chat is not shown here** (AI schema
                // 13): the host opens it in a window of its own profile, and
                // this tab becomes a new chat.
                let transcript = match transcript {
                    Some(transcript) if self.belongs_elsewhere(&transcript.chat) => {
                        self.refuse_chat_elsewhere(&transcript.chat);
                        None
                    }
                    transcript => transcript,
                };
                self.waiting = false;
                self.apply_settings_fields(&settings);
                self.setup_provider = selected_setup_provider(&settings);
                self.setup_agent = settings
                    .selected_provider
                    .clone()
                    .filter(agents::is_agent_provider);
                // The chat view either way (the setup is dormant): a chat
                // is made once the chosen provider validates, and with
                // none chosen the chip asks for a model.
                self.validated_ready = false;
                let selected = settings.selected_provider.clone();
                self.settings = Some(settings);
                self.chats = chats;
                self.keep_profile_chats();
                self.destinations = destinations;
                self.prioritize_context_workspace();
                self.workspace_catalog_error = destination_error;
                self.selected_destination = self
                    .selected_destination
                    .min(self.destinations.len().saturating_sub(1));
                self.scroll_loaded_chat_to_bottom = transcript.is_some();
                self.transcript = transcript;
                self.prime_skill_list();
                self.messages_dirty = true;
                // A saved OpenAI credential is enough to discover the
                // account-scoped model catalog, even when another provider is
                // currently selected. This makes the picker ready when
                // Settings first opens instead of after a provider toggle.
                self.validate_models_after_settings(selected);
            }
            Update::Created(mut transcript) => {
                self.fresh_chat_wanted = false;
                // **The new chat on screen, made for its first message**: the
                // message is already on its way behind the create, so its card,
                // the dots and the wait stay. What was chosen on the stand-in
                // shows until its own update, queued behind the create, lands.
                let made = self
                    .transcript
                    .as_ref()
                    .filter(|shown| shown.chat.id == transcript.chat.id)
                    .cloned();
                if let Some(stand_in) = &made {
                    transcript.chat.private = stand_in.chat.private;
                    transcript.chat.agent_working_folder =
                        stand_in.chat.agent_working_folder.clone();
                    transcript.tool_policy = stand_in.tool_policy.clone();
                    if transcript.reasoning_control.is_none() {
                        transcript.reasoning_control = stand_in.reasoning_control.clone();
                    }
                }
                if made.is_none() {
                    self.forget_input_walk();
                    self.waiting = false;
                    self.attempt = None;
                    self.pending_assistant_message_id = None;
                    self.pending_user = None;
                    self.answering = false;
                    self.pulse = None;
                    self.live_usage = None;
                    self.live_sources.clear();
                    self.open_searches.clear();
                    self.draft.clear();
                    self.pending_question_anchor = None;
                    self.scroll_to_pending_question = false;
                }
                let chat = transcript.chat.id;
                self.remember_chat(&transcript.chat, true);
                self.transcript = Some(transcript);
                self.prime_skill_list();
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.scroll_loaded_chat_to_bottom = false;
                self.messages_dirty = true;
                if made.is_some() {
                    self.waiting = self.message_on_its_way();
                }
                if self.paste_after_create.take() == Some(chat) {
                    self.begin_attachment_paste();
                }
                if self
                    .pending_computer_reference
                    .as_ref()
                    .is_some_and(|(pending, _, _)| *pending == chat)
                    && let Some((_, path, kind)) = self.pending_computer_reference.take()
                {
                    self.add_draft(chat, attachments::Draft::reference(path, kind));
                }
                self.insert_completed_workspace_pills(chat);
            }
            Update::CreateFailed(chat, failure) => {
                // The chat on screen is a stand-in again, so the next send
                // asks for it again; the message queued behind the create
                // comes back to the composer with this reason.
                if self.shown_chat_id() == Some(chat) {
                    self.unmade_chat = Some(chat);
                }
                if self.paste_after_create == Some(chat) {
                    self.paste_after_create = None;
                }
                self.waiting = false;
                self.status = Some(failure_text(&failure, self.lang));
            }
            Update::Listed(chats) => {
                self.chats = chats;
                self.keep_profile_chats();
            }
            Update::Transcript(transcript) if self.belongs_elsewhere(&transcript.chat) => {
                self.waiting = false;
                self.refuse_chat_elsewhere(&transcript.chat);
            }
            Update::Transcript(transcript) => {
                self.sent_inputs.retain(|sent| {
                    sent.chat != transcript.chat.id
                        || sent.message_id.is_none_or(|id| {
                            !transcript.messages.iter().any(|message| message.id == id)
                        })
                });
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.answering = false;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.live_reasoning.clear();
                self.reasoning_expanded = false;
                self.draft.clear();
                let changed_chat = self
                    .transcript
                    .as_ref()
                    .is_none_or(|current| current.chat.id != transcript.chat.id);
                if changed_chat || self.scroll_loaded_chat_to_bottom {
                    self.pending_question_anchor = None;
                    self.scroll_to_pending_question = false;
                }
                self.remember_chat(&transcript.chat, false);
                if changed_chat {
                    self.forget_input_walk();
                    self.live_offsets.clear();
                    self.web_search_armed = true;
                }
                self.transcript = Some(transcript);
                self.prime_skill_list();
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.messages_dirty = true;
                // The read carries every steered message of the reply.
                let read = self.transcript.as_ref().map(|t| t.chat.id);
                self.steered.retain(|(chat, _)| Some(*chat) != read);
                if let Some(chat) = read {
                    if changed_chat {
                        self.queue_shown_again(chat);
                    }
                    self.insert_completed_workspace_pills(chat);
                    self.after_reply(chat);
                }
            }
            Update::Renamed(chat) | Update::Pinned(chat) => {
                self.waiting = false;
                // In place: a rename or a pin is not activity, and the list
                // stays as it was ordered (the user's call, 2026-10-31).
                self.remember_chat(&chat, false);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat;
                }
            }
            Update::ModelUpdated(chat, reasoning_control) => {
                let previous_skill_source = self.skill_source();
                let chat_id = chat.id;
                self.waiting = false;
                // The picker stays open over a model chosen in it (D66): its
                // footer becomes the landed model's, and the list is kept
                // in range around the chosen row. Not promoted: only a
                // message is activity (D85), and the service leaves
                // `updated_at` alone.
                self.remember_chat(&chat, false);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat;
                    transcript.reasoning_control = reasoning_control;
                }
                if self.skill_source() != previous_skill_source {
                    if let Some(transcript) = &mut self.transcript {
                        transcript.agent_commands = None;
                    }
                    self.prime_skill_list();
                    self.sync_skill_list();
                }
                self.settle_after_choice();
                self.status =
                    Some(koru_i18n::t(self.lang, koru_i18n::Key::AiModelNextTurn).to_owned());
                self.drop_strip_if_not_agent(chat_id);
            }
            Update::PrivacyUpdated(chat) => {
                self.waiting = self.message_on_its_way();
                // Not activity (D85): the chat keeps its place.
                self.remember_chat(&chat, false);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat.clone();
                }
                // Behind a new chat's create, it only repeats the stand-in's
                // choice; the message is what the person is watching.
                if !self.message_on_its_way() {
                    self.status = Some(if chat.private {
                        "This chat is private to you.".into()
                    } else {
                        "Agents and connected assistants can read this chat.".into()
                    });
                }
            }
            Update::AgentSettingsUpdated(chat) => {
                self.waiting = self.message_on_its_way();
                // Not promoted: a mode is no activity, and the list keeps
                // its order (the service leaves `updated_at` alone too).
                self.remember_chat(&chat, false);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat.clone();
                }
                self.status = None;
            }
            Update::FolderUpdated(chat) => {
                self.waiting = self.message_on_its_way();
                // Not activity (D85): the chat keeps its place.
                self.remember_chat(&chat, false);
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == chat.id
                {
                    transcript.chat = chat.clone();
                }
                self.status = None;
            }
            Update::ToolPolicy(policy) => {
                self.waiting = self.message_on_its_way();
                if let Some(transcript) = &mut self.transcript
                    && transcript.chat.id == policy.chat_id
                {
                    transcript.tool_policy = policy;
                }
                if !self.message_on_its_way() {
                    self.status = Some("Workspace access updated for this chat.".into());
                }
            }
            Update::GrantsAdded { chat, op, result } => {
                if let Some((pending_op, request)) = self.pending_workspace_import_grant.take() {
                    if pending_op == op && request.chat == chat {
                        match result {
                            Ok(policy) => {
                                if let Some(transcript) = &mut self.transcript
                                    && transcript.chat.id == chat
                                {
                                    transcript.tool_policy = policy;
                                }
                                if let Some(document) = self
                                    .workspace_import_documents
                                    .get(&request.attachment)
                                    .copied()
                                {
                                    self.copy_imported_workspace_file(&request, document);
                                } else {
                                    self.workspace_import_request = Some(request);
                                }
                            }
                            Err(message) => {
                                tracing::debug!(target: "koru_ai::ui", op = %op, reason = %message, "workspace grant for file failed");
                                if let Some(draft) = self.strips.draft_mut(chat, request.attachment)
                                {
                                    draft.state =
                                        attachments::DraftState::Failed { retryable: true };
                                }
                                self.status = Some(
                                    koru_i18n::t(self.lang, koru_i18n::Key::AiComputerGrantFailed)
                                        .to_owned(),
                                );
                            }
                        }
                        return true;
                    }
                    self.pending_workspace_import_grant = Some((pending_op, request));
                }
                let Some((key, pending_op)) = self.pending_grants.take() else {
                    return false;
                };
                if pending_op != op || key.chat != chat {
                    self.pending_grants = Some((key, pending_op));
                    return false;
                }
                self.waiting = self.message_on_its_way();
                if self.shown_chat_id() != Some(chat) {
                    return true;
                }
                match result {
                    Ok(policy) => {
                        if let Some(transcript) = &mut self.transcript {
                            transcript.tool_policy = policy;
                        }
                        let current = self.composer.message();
                        let chips: Vec<_> = current.documents.iter().map(|(id, _)| *id).collect();
                        let documents: Vec<_> = self
                            .turn_refs(&chips)
                            .0
                            .iter()
                            .filter_map(|reference| match reference {
                                ContextRef::Document { document_id } => Some(*document_id),
                                ContextRef::MessageBlock { .. } => None,
                            })
                            .collect();
                        if current.markdown.trim() == key.markdown
                            && self.context_workspace == key.current
                            && documents == key.documents
                            && self
                                .strip()
                                .iter()
                                .map(|draft| draft.id)
                                .collect::<Vec<_>>()
                                == key.attachments
                        {
                            self.grants_ready = Some(key);
                            self.send_composer();
                        }
                    }
                    Err(message) => self.status = Some(message),
                }
            }
            Update::Restaged { new_id, result } => {
                if let Some(chat) = self.recall_restages.get(&new_id).copied() {
                    if let Some(draft) = self.strips.draft_mut(chat, new_id) {
                        match result {
                            Ok(attachment) if attachment.id == new_id => {
                                draft.state = attachments::DraftState::Stored;
                                draft.byte_count = attachment.byte_count;
                                draft.size = attachment.dimensions;
                            }
                            Ok(_) => {
                                draft.state = attachments::DraftState::Failed { retryable: false };
                                self.status = Some(
                                    koru_i18n::t(self.lang, koru_i18n::Key::AiAttachmentFailed)
                                        .to_owned(),
                                );
                                self.attachment_work.remove(chat, new_id);
                            }
                            Err(message) => {
                                draft.state = attachments::DraftState::Failed { retryable: false };
                                self.status = Some(message);
                            }
                        }
                    } else if result.is_ok() {
                        self.attachment_work.remove(chat, new_id);
                    }
                } else if let Some(chat) = self.recall_retired.remove(&new_id)
                    && result.is_ok()
                {
                    self.attachment_work.remove(chat, new_id);
                }
            }
            Update::AttachmentCopied { chat, id, result } => {
                let request = self.workspace_imports.get(&id).cloned();
                let copied = result
                    .as_ref()
                    .ok()
                    .map(|attachment| attachment.file_name.clone());
                let copy_failed = result.is_err();
                if let Some(request) = request {
                    self.on_stored(request.chat, id, result);
                    if copy_failed && self.shown_chat_id() == Some(request.chat) {
                        self.status = Some(
                            koru_i18n::t(self.lang, koru_i18n::Key::AiComputerCopyFailed)
                                .to_owned(),
                        );
                    }
                    if let Some(title) = copied {
                        self.workspace_imports.remove(&id);
                        if let Some(document) = self.workspace_import_documents.remove(&id) {
                            if self.shown_chat_id() == Some(request.chat) {
                                self.composer.insert_document(document, &title);
                            } else {
                                self.completed_workspace_pills
                                    .entry(request.chat)
                                    .or_default()
                                    .push((document, title));
                            }
                        }
                    }
                } else if result.is_ok()
                    && !self.strips.get(chat).iter().any(|draft| draft.id == id)
                {
                    self.attachment_work.remove(chat, id);
                }
            }
            Update::ToolActivity(activity) => {
                // A decision response is not the end of the provider turn;
                // keep polling the stream until its terminal event arrives.
                self.waiting = self.attempt.is_some();
                self.upsert_tool_activity(activity);
            }
            Update::Usage(report) => {
                self.waiting = false;
                self.usage_report = Some(report);
                self.messages_dirty = true;
                self.status = Some("Private local usage — no provider was contacted.".into());
            }
            Update::HistoryChanged { chats, transcript } => {
                // A tab that asked for a new chat does not take the
                // service's current one (the review's note): the snapshot's
                // chat is dropped and a chat is created below.
                let transcript = if self.chat_unmade() {
                    // A new chat not made yet stays on screen: deleting or
                    // clearing others does not take it away.
                    self.transcript.clone()
                } else if self.fresh_chat_wanted {
                    None
                } else {
                    transcript
                };
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.answering = false;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.draft.clear();
                if self.transcript.as_ref().map(|t| t.chat.id)
                    != transcript.as_ref().map(|t| t.chat.id)
                {
                    self.forget_input_walk();
                }
                self.chats = chats;
                self.transcript = transcript;
                self.prime_skill_list();
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.pending_question_anchor = None;
                self.scroll_to_pending_question = false;
                self.messages_dirty = true;
                if self.transcript.is_none() && self.validated_ready {
                    self.create_chat();
                }
            }
            Update::Accepted(accepted) => {
                if let Some(sent) = self
                    .sent_inputs
                    .iter_mut()
                    .rev()
                    .find(|sent| sent.message_id.is_none())
                {
                    sent.message_id = Some(accepted.user_message_id);
                }
                self.sent_message = None;
                self.attempt = Some(accepted.attempt_id);
                self.pending_assistant_message_id = Some(accepted.assistant_message_id);
                self.live_turn_started_ms = Some(koru_ai_core::Timestamp::now().millis());
                self.messages_dirty = true;
                // Sent: the chat moves up its list (D85).
                self.note_activity();
            }
            Update::Event(event) => {
                let terminal = event.event.is_terminal();
                match &event.event {
                    PublicAiEvent::TextDelta { text, .. } => {
                        self.pulse = None;
                        self.draft.push_str(text);
                    }
                    PublicAiEvent::ReasoningDelta { text, .. } => {
                        self.live_reasoning.push_str(text);
                        self.reasoning_expanded = true;
                    }
                    PublicAiEvent::Usage { usage, .. } => {
                        self.live_usage = Some(usage.clone());
                    }
                    PublicAiEvent::ToolActivity { activity, .. } => {
                        self.upsert_tool_activity(activity.clone());
                    }
                    PublicAiEvent::AgentNotice { notice, .. } => {
                        self.upsert_agent_notice(notice.clone());
                    }
                    PublicAiEvent::AgentCommands { chat, commands, .. } => {
                        if let Some(transcript) = self.transcript.as_mut()
                            && transcript.chat.id == *chat
                        {
                            transcript.agent_commands = Some(commands.clone());
                            self.sync_skill_list();
                        }
                    }
                    PublicAiEvent::Notice { notice, .. } => {
                        self.status = Some(match notice {
                            koru_ai_core::AttemptNotice::FreshStart => {
                                koru_i18n::t(self.lang, koru_i18n::Key::AiAgentStartedFresh)
                                    .to_owned()
                            }
                            koru_ai_core::AttemptNotice::ImagesAsFiles => {
                                koru_i18n::t(self.lang, koru_i18n::Key::AiAgentImagesAsFiles)
                                    .to_owned()
                            }
                        });
                    }
                    PublicAiEvent::WebSearch { call_id, phase, .. } => match phase {
                        WebSearchPhase::Started => {
                            self.open_searches.insert(call_id.clone());
                        }
                        WebSearchPhase::Completed => {
                            self.open_searches.remove(call_id);
                        }
                    },
                    PublicAiEvent::Citation { source, .. } => {
                        if !self
                            .live_sources
                            .iter()
                            .any(|existing| existing.url == source.url)
                        {
                            self.live_sources.push(source.clone());
                        }
                    }
                    PublicAiEvent::Failed { error, .. } => {
                        // A refused level is said in the person's language.
                        self.status = Some(
                            if error.public_message == koru_ai_core::AGENT_REASONING_REJECTED {
                                koru_i18n::t(self.lang, koru_i18n::Key::AiAgentReasoningRejected)
                                    .to_owned()
                            } else if error.public_message == koru_ai_core::AGENT_QUESTIONS_CLOSED {
                                // Questions whose agent's session ended (D108).
                                koru_i18n::t(self.lang, koru_i18n::Key::AiQuestionsPassedOver)
                                    .to_owned()
                            } else {
                                error.public_message.clone()
                            },
                        );
                    }
                    _ => {}
                }
                if terminal {
                    if let Some(chat) = self.shown_chat_id() {
                        let end = match &event.event {
                            PublicAiEvent::Completed { .. } => queue::ReplyEnd::Completed,
                            PublicAiEvent::Cancelled { .. } => queue::ReplyEnd::Stopped,
                            _ => queue::ReplyEnd::Failed,
                        };
                        self.reply_end = Some((chat, end));
                    }
                    self.ended_attempt = self.attempt;
                    self.replies_finished = self.replies_finished.wrapping_add(1);
                    // Its reply arrived: the chat moves up again (D85).
                    self.note_activity();
                    self.pulse = None;
                    self.open_searches.clear();
                    self.reasoning_expanded = false;
                }
                self.messages_dirty = true;
                if terminal {
                    if let Some(chat) = self.transcript.as_ref().map(|t| t.chat.id) {
                        self.send_command(Command::Read(chat));
                    } else {
                        self.waiting = false;
                    }
                }
            }
            Update::RecoveryStarted(attempt) => {
                if self.attempt == Some(attempt) {
                    self.waiting = true;
                    self.status =
                        Some("Connection interrupted. Recovering this response...".into());
                }
            }
            Update::Recovered {
                attempt,
                status,
                transcript,
            } => {
                if self.attempt != Some(attempt) {
                    tracing::debug!(
                        target: "koru_ai::ui_stream",
                        attempt = %attempt,
                        status = ?status,
                        "AI UI ignored recovery for an attempt that is no longer active"
                    );
                    return true;
                }
                self.replies_finished = self.replies_finished.wrapping_add(1);
                if let Some(chat) = self.shown_chat_id() {
                    self.reply_end = Some((
                        chat,
                        match status {
                            koru_ai_core::AttemptStatus::Complete => queue::ReplyEnd::Completed,
                            koru_ai_core::AttemptStatus::Cancelled => queue::ReplyEnd::Stopped,
                            _ => queue::ReplyEnd::Failed,
                        },
                    ));
                }
                let changed = self.apply_update(Update::Transcript(transcript));
                self.status = match status {
                    koru_ai_core::AttemptStatus::Complete => None,
                    koru_ai_core::AttemptStatus::Interrupted => {
                        Some("This response was interrupted. You can retry it.".into())
                    }
                    koru_ai_core::AttemptStatus::Failed => {
                        Some("This response failed. You can retry it.".into())
                    }
                    koru_ai_core::AttemptStatus::Cancelled => Some("Response stopped.".into()),
                    koru_ai_core::AttemptStatus::Pending
                    | koru_ai_core::AttemptStatus::Streaming => {
                        Some("Connection interrupted. Recovering this response...".into())
                    }
                };
                return changed;
            }
            Update::RecoveryFailed { attempt, message } => {
                if self.attempt != Some(attempt) {
                    tracing::debug!(
                        target: "koru_ai::ui_stream",
                        attempt = %attempt,
                        "AI UI ignored a recovery failure for an attempt that is no longer active"
                    );
                    return true;
                }
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.answering = false;
                self.pulse = None;
                self.live_usage = None;
                self.live_sources.clear();
                self.open_searches.clear();
                self.draft.clear();
                self.pending_question_anchor = None;
                self.scroll_to_pending_question = false;
                self.messages_dirty = true;
                self.status = Some(message);
                if let Some(chat) = self.shown_chat_id() {
                    self.steer_after_cancel = None;
                    self.queues.pause(chat);
                }
            }
            Update::Settings(settings) => {
                self.waiting = false;
                self.validated_ready = false;
                self.apply_settings_fields(&settings);
                if let Some(agent) = settings
                    .selected_provider
                    .clone()
                    .filter(agents::is_agent_provider)
                {
                    self.setup_agent = Some(agent);
                }
                let selected = settings.selected_provider.clone();
                self.settings = Some(settings);
                self.validate_models_after_settings(selected);
                self.prefetch_agent_models();
            }
            Update::AgentModels(response) => {
                self.agent_models_asked.remove(&response.provider);
                self.agent_models_answered(&response.provider);
                // The agent's approval modes come with its models (D7).
                if response.modes.is_empty() {
                    self.agent_modes.remove(&response.provider);
                } else {
                    self.agent_modes.insert(
                        response.provider.clone(),
                        approval_modes::ListedModes {
                            modes: response.modes.clone(),
                            default: response.default_mode.clone(),
                        },
                    );
                }
                // A new chat begun before its agent's modes were known
                // starts from its default now (D39).
                self.adopt_default_mode(&response.provider);
                // Whether a begun chat's level can still change (D70).
                if response.levels_per_turn {
                    self.levels_per_turn.insert(response.provider.clone());
                } else {
                    self.levels_per_turn.remove(&response.provider);
                }
                // And its model, to another of the agent's (protocol v22).
                if response.models_per_turn {
                    self.models_per_turn.insert(response.provider.clone());
                } else {
                    self.models_per_turn.remove(&response.provider);
                }
                self.agent_models.insert(
                    response.provider,
                    agents::AgentModels::Listed(response.models),
                );
            }
            Update::AgentCommands {
                provider,
                folder,
                commands,
            } => {
                self.skills_listed(provider, folder, commands);
            }
            Update::AgentModelsPending(provider) => {
                // Listing models… stays, as does a list already on screen;
                // the next open of the picker asks again.
                self.agent_models_asked.remove(&provider);
                self.agent_models_answered(&provider);
            }
            Update::AgentModelsFailed(provider) => {
                self.agent_models_asked.remove(&provider);
                self.agent_models_answered(&provider);
                // A list already on screen stays: the next open asks again.
                if !matches!(
                    self.agent_models.get(&provider),
                    Some(agents::AgentModels::Listed(_))
                ) {
                    self.agent_models
                        .insert(provider, agents::AgentModels::Failed);
                }
            }
            Update::Validation(validation) => {
                tracing::info!(
                    target: "koru_ai::ui",
                    provider = %validation.provider,
                    status = ?validation.status,
                    model_count = validation.models.len(),
                    "AI UI applied provider validation"
                );
                self.waiting = false;
                match validation.provider.as_str() {
                    AiProviderId::OPENAI => self.openai_models = validation.models.clone(),
                    AiProviderId::OLLAMA => self.ollama_models = validation.models.clone(),
                    _ => {}
                }
                if agents::is_agent_provider(&validation.provider) {
                    let status = AgentStatus::from_validation(&validation);
                    let was = self
                        .agent_status
                        .insert(validation.provider.clone(), status);
                    // An agent that turns ready while the picker is open
                    // has a list worth asking for again.
                    if self.chat_model_open
                        && status == AgentStatus::Ready
                        && was != Some(AgentStatus::Ready)
                    {
                        self.ask_agent_models(validation.provider.clone());
                    }
                    self.messages_dirty = true;
                    if self.setup_provider == SetupProvider::Agents
                        && self.setup_agent.as_ref() == Some(&validation.provider)
                        && !status.selectable()
                        && (self.setup_open || self.awaiting_model())
                    {
                        self.status = self.agent_not_ready_line(&validation.provider);
                    }
                }
                let current_provider = self.setup_provider_id();
                if current_provider.as_ref() != Some(&validation.provider) {
                    if let Some(provider) = self.pending_model_prefetch.take() {
                        self.send_model_prefetch(provider);
                    }
                    return true;
                }
                let validates_selected_provider = self
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.selected_provider.as_ref())
                    .is_some_and(|selected| selected == &validation.provider);
                if validates_selected_provider {
                    // Readiness describes the persisted provider used for the
                    // next turn, not whichever setup card happens to be open.
                    // A daemon or catalog that disappears during the session
                    // must revoke an earlier successful validation.
                    self.validated_ready = validation.status == ProviderValidationStatus::Ready;
                    // A tab that still owes the fresh chat it was opened
                    // for gets it below, where a ready provider makes the
                    // chat any surface without one is waiting for. (It was
                    // also made here, so such a tab asked for two.)
                }
                self.constrain_reasoning(&validation);
                let mut discovered_model = false;
                if validation.provider.as_str() == AiProviderId::OPENAI
                    && !validation.models.is_empty()
                    && self.openai_model.is_empty()
                {
                    if let Some(model) = preferred_openai_model(&validation.models) {
                        self.openai_model.set_text(&mut self.engine, model);
                        self.model_options_open = true;
                        discovered_model = true;
                    }
                }
                if validation.status == ProviderValidationStatus::Ready
                    && validates_selected_provider
                {
                    self.setup_open = false;
                    self.status = None;
                    if self.transcript.is_none() {
                        self.create_chat();
                    }
                } else if !self.setup_open {
                    // **The chat view says only why the chosen provider
                    // cannot make the chat it is waiting for** — the one
                    // line, above the composer whose chip asks for a model.
                    // Advice about saving, or about a provider nobody
                    // chose, was the setup's to give.
                    if validates_selected_provider && self.awaiting_model() {
                        self.status = Some(self.validation_line(&validation));
                    }
                } else if validation.status == ProviderValidationStatus::Ready {
                    self.status = Some("Provider ready. Save & check to use it.".into());
                } else if discovered_model {
                    self.status = Some("Choose an available model, then Save & check.".into());
                } else {
                    self.status = Some(self.validation_line(&validation));
                }
                self.validation = Some(validation);
                if let Some(provider) = self.pending_model_prefetch.take() {
                    self.send_model_prefetch(provider);
                }
            }
            Update::Pull(event) => {
                self.pull_status = Some(match event {
                    ModelPullEvent::Progress { status, .. } => {
                        self.pull_active = true;
                        status
                    }
                    ModelPullEvent::Completed { model } => {
                        self.pull_active = false;
                        self.waiting = false;
                        let provider = AiProviderId::parse(AiProviderId::OLLAMA)
                            .expect("built-in provider id");
                        self.send_command(Command::Validate(provider));
                        format!("{model} is ready")
                    }
                    ModelPullEvent::Failed { failure } => {
                        self.pull_active = false;
                        self.waiting = false;
                        failure_text(&failure, self.lang)
                    }
                    ModelPullEvent::Cancelled { model } => {
                        self.pull_active = false;
                        self.waiting = false;
                        format!("Stopped pulling {model}")
                    }
                });
            }
            Update::DocumentTitles(titles) => {
                self.titles_in_flight = false;
                // Another command may still be streaming; keep polling for it.
                self.waiting = self.attempt.is_some();
                let mut changed = false;
                for (document, title) in titles {
                    if !title.trim().is_empty()
                        && self.document_titles.get(&document) != Some(&title)
                    {
                        self.document_titles.insert(document, title);
                        changed = true;
                    }
                }
                // The shown text of every message naming it changes, so the
                // layout cache relays those cards and no others.
                self.messages_dirty |= changed;
            }
            Update::Exported(title, document) => {
                self.waiting = false;
                self.destination_open = None;
                let line = koru_i18n::fill(
                    koru_i18n::t(self.lang, koru_i18n::Key::AiStoredInKoru),
                    &[&title],
                );
                self.stored_document = document.map(|document| (line.clone(), document));
                self.status = Some(line);
            }
            Update::Steered(response) => {
                tracing::debug!(
                    target: "koru_ai::ui",
                    replayed = response.replayed,
                    "AI UI steer was taken by the running reply"
                );
                self.waiting = self.attempt.is_some();
                self.steer_accepted(response.user_message_id);
            }
            Update::SteerFailed(failure) => {
                self.waiting = self.attempt.is_some();
                self.steer_refused(&failure);
            }
            Update::SendFailed(failure) => {
                tracing::warn!(
                    target: "koru_ai::ui",
                    code = failure.code,
                    "AI UI send was not accepted; the message is back in the composer"
                );
                if self.chat_unmade() {
                    // The create failed and said why; the send only follows.
                    self.give_back_sent(None);
                } else {
                    self.give_back_sent(Some(&failure));
                }
            }
            // A command queued behind a create that failed: the create
            // has said why.
            Update::Failure(_) if self.chat_unmade() => {}
            Update::Failure(message) => {
                self.settle_question_cards(true);
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI received a client failure"
                );
                if self.pull_active {
                    self.pull_active = false;
                    self.pull_status = None;
                }
                // **A failed side command is not the reply's end.** A
                // rename, the automatic title, a policy, an approval or a
                // stop that fails while a reply streams leaves the reply
                // streaming: its attempt — and so Stop — and its id stay,
                // and its own terminal event or recovery clears them. A
                // send that fails is `SendFailed`, which gives it back.
                if self.reply_streaming() {
                    self.waiting = true;
                } else {
                    self.waiting = false;
                    self.attempt = None;
                    self.pending_assistant_message_id = None;
                    self.live_sources.clear();
                    self.open_searches.clear();
                }
                self.status = Some(message);
            }
        }
        true
    }

    fn insert_completed_workspace_pills(&mut self, chat: ChatId) {
        if self.shown_chat_id() != Some(chat) {
            return;
        }
        if let Some(pills) = self.completed_workspace_pills.remove(&chat) {
            for (document, title) in pills {
                self.composer.insert_document(document, &title);
            }
        }
    }

    fn remember_chat(&mut self, chat: &Chat, promote: bool) {
        if let Some(index) = self
            .chats
            .iter()
            .position(|candidate| candidate.id == chat.id)
        {
            self.chats[index] = chat.clone();
            if promote && index != 0 {
                let chat = self.chats.remove(index);
                self.chats.insert(0, chat);
            }
        } else if promote {
            self.chats.insert(0, chat.clone());
        } else {
            self.chats.push(chat.clone());
        }
    }

    /// A local agent's notice as it comes (koru-ai D69): kept with the
    /// transcript, once, and laid out in front of its reply.
    fn upsert_agent_notice(&mut self, notice: koru_ai_core::AgentNotice) {
        let Some(transcript) = &mut self.transcript else {
            return;
        };
        if transcript.chat.id != notice.chat_id
            || transcript
                .notices
                .iter()
                .any(|existing| existing.id == notice.id)
        {
            return;
        }
        transcript.notices.push(notice);
        self.messages_dirty = true;
    }

    fn upsert_tool_activity(&mut self, mut activity: ToolActivity) {
        let activity_id = activity.id;
        let Some(transcript) = &mut self.transcript else {
            return;
        };
        if transcript.chat.id != activity.chat_id {
            return;
        }
        let arrived_new = !transcript
            .tool_activities
            .iter()
            .any(|existing| existing.id == activity.id);
        let (reply, unplaced) = (
            activity.assistant_message_id,
            activity.text_offset.is_none() && arrived_new,
        );
        if let Some(existing) = transcript
            .tool_activities
            .iter_mut()
            .find(|existing| existing.id == activity.id)
        {
            if activity.display_target.is_none() {
                activity.display_target.clone_from(&existing.display_target);
            }
            *existing = activity;
        } else {
            transcript.tool_activities.push(activity);
            transcript
                .tool_activities
                .sort_by_key(|activity| activity.proposed_at);
        }
        // Where a call of the running reply began, as far as the draft has
        // come; the stored offset replaces it once the row carries one.
        if unplaced && self.pending_assistant_message_id == Some(reply) {
            self.live_offsets.insert(
                activity_id,
                u32::try_from(self.draft.len()).unwrap_or(u32::MAX),
            );
        }
        // A set of questions that just arrived may take the keyboard (D108).
        let arrived = arrived_new
            .then(|| {
                self.transcript.as_ref().and_then(|transcript| {
                    transcript
                        .tool_activities
                        .iter()
                        .find(|activity| activity.id == activity_id)
                        .cloned()
                })
            })
            .flatten();
        if let Some(activity) = arrived {
            self.offer_question_keyboard(&activity);
        }
        self.messages_dirty = true;
    }

    /// Whether a reply is streaming: a turn was accepted and its stream has
    /// not yet sent its terminal event.
    fn reply_streaming(&self) -> bool {
        self.attempt.is_some() && self.attempt != self.ended_attempt
    }

    /// A reply sent from here is on its way or running: the person's
    /// message, or an agent's answers sent as one (D108).
    fn reply_pending(&self) -> bool {
        self.pending_user.is_some() || self.answering
    }

    fn update_tool_policy(&mut self, mode: ToolMode, workspace_ids: Vec<WorkspaceId>) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        self.send_command(Command::UpdateToolPolicy(
            chat,
            UpdateChatToolPolicyRequest {
                mode,
                workspace_ids,
            },
        ))
    }

    fn prioritize_context_workspace(&mut self) {
        let Some(current) = self.context_workspace else {
            return;
        };
        let Some(index) = self
            .destinations
            .iter()
            .position(|destination| destination.workspace_id == current)
        else {
            return;
        };
        if index != 0 {
            let current = self.destinations.remove(index);
            self.destinations.insert(0, current);
        }
    }

    fn toggle_tool_workspace(&mut self, workspace: WorkspaceId) -> bool {
        let Some(policy) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.tool_policy.clone())
        else {
            return false;
        };
        let mut workspaces = policy.workspace_ids;
        if let Some(index) = workspaces
            .iter()
            .position(|candidate| *candidate == workspace)
        {
            workspaces.remove(index);
        } else {
            workspaces.push(workspace);
            workspaces.sort();
        }
        self.update_tool_policy(policy.mode, workspaces)
    }

    fn create_chat(&mut self) -> bool {
        let Some(selection) = self.current_selection() else {
            // **Nothing chosen yet: the chat view waits for a model.** No
            // setup opens; the chip asks for one, and the composer's picker
            // makes the chat once it is chosen ([`Self::start_chat_on`]).
            // A chat being shown is left for the new one, which is owed.
            self.fresh_chat_wanted = true;
            if self.transcript.take().is_some() {
                self.forget_input_walk();
                self.usage_report = None;
                self.question_navigation = None;
                self.question_navigation_anchor = None;
                self.messages_dirty = true;
            }
            return true;
        };
        self.begin_unmade_chat(selection);
        true
    }

    /// **Show a new chat on `model` without making it**: a stand-in
    /// transcript that only this surface knows. Choosing its model, folder,
    /// privacy or workspace access changes the stand-in
    /// ([`Self::apply_to_unmade_chat`]); its first message makes the chat
    /// ([`Self::make_unmade_chat`]). Leaving it unsent leaves nothing behind.
    fn begin_unmade_chat(&mut self, model: ModelSelection) {
        let mut chat = unmade_transcript(model);
        // A new chat with an agent starts from the person's default
        // permission, mapped onto the agent's own modes (D39).
        chat.chat.agent_mode = self.default_approval_mode(&chat.chat.current_model.provider);
        self.approval_mode_open = false;
        let id = chat.chat.id;
        self.fresh_chat_wanted = false;
        self.forget_input_walk();
        self.usage_report = None;
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        self.pending_question_anchor = None;
        self.scroll_to_pending_question = false;
        self.scroll_loaded_chat_to_bottom = false;
        self.transcript = Some(chat);
        self.prime_skill_list();
        self.unmade_chat = Some(id);
        self.messages_dirty = true;
    }

    /// A message sent and not yet accepted: the surface keeps polling for
    /// its answer whatever else lands first.
    fn message_on_its_way(&self) -> bool {
        self.sent_message.is_some() || self.staged_embedded_send.is_some()
    }

    /// Whether the chat on screen is a stand-in not made yet.
    fn chat_unmade(&self) -> bool {
        self.unmade_chat.is_some() && self.unmade_chat == self.shown_chat_id()
    }

    /// A command about the chat not made yet changes the stand-in instead of
    /// reaching the service, which has no such chat; `None` when it was
    /// applied here. Anything else is handed back to be queued.
    fn apply_to_unmade_chat(&mut self, command: Command) -> Option<Command> {
        if !self.chat_unmade() {
            return Some(command);
        }
        let unmade = self.unmade_chat?;
        let transcript = self.transcript.as_mut()?;
        match command {
            Command::UpdateModel(chat, request) if chat == unmade => {
                let provider_changed =
                    transcript.chat.current_model.provider != request.model.provider;
                transcript.chat.current_model = request.model;
                if provider_changed {
                    transcript.reasoning_control = None;
                    // Fast mode belongs to the model it was chosen on.
                    transcript.chat.agent_fast = None;
                    // Another agent: its default permission (D39).
                    let agent = transcript.chat.current_model.provider.clone();
                    let mode = self.default_approval_mode(&agent);
                    if let Some(transcript) = self.transcript.as_mut() {
                        transcript.chat.agent_mode = mode;
                    }
                }
            }
            Command::UpdateAgentSettings(chat, request) if chat == unmade => {
                if let Some(mode) = request.mode {
                    transcript.chat.agent_mode = Some(mode);
                }
                if let Some(fast) = request.fast {
                    transcript.chat.agent_fast = Some(fast);
                }
            }
            Command::UpdatePrivacy(chat, request) if chat == unmade => {
                transcript.chat.private = request.private;
            }
            Command::UpdateFolder(chat, request) if chat == unmade => {
                transcript.chat.agent_working_folder = request.folder;
            }
            Command::UpdateToolPolicy(chat, request) if chat == unmade => {
                transcript.tool_policy.mode = request.mode;
                transcript.tool_policy.workspace_ids = request.workspace_ids;
            }
            Command::Rename(chat, request) if chat == unmade => {
                transcript.chat.title = request.title;
            }
            // Nothing to read back: the stand-in is all there is.
            Command::Read(chat) if chat == unmade => {}
            command => return Some(command),
        }
        self.messages_dirty = true;
        None
    }

    /// **Make the chat on screen if it is a stand-in**, for the first message
    /// about to be sent: the create, then what was chosen on the stand-in
    /// that the create does not carry. The reader answers them in order, so
    /// everything queued after — the message, its title — finds the chat
    /// made. `false` when the create could not be queued; the message stays
    /// in the composer.
    fn make_unmade_chat(&mut self) -> bool {
        if !self.chat_unmade() {
            return true;
        }
        let Some(stand_in) = self.transcript.clone() else {
            return true;
        };
        let chat = stand_in.chat.id;
        // Cleared first, so what follows is queued rather than kept here.
        self.unmade_chat = None;
        if !self.queue_command(Command::Create(CreateChatRequest {
            chat_id: chat,
            title: stand_in.chat.title.clone(),
            profile_id: self.profile,
            model: stand_in.chat.current_model.clone(),
        })) {
            self.unmade_chat = Some(chat);
            return false;
        }
        if stand_in.chat.agent_mode.is_some() || stand_in.chat.agent_fast.is_some() {
            self.queue_command(Command::UpdateAgentSettings(
                chat,
                koru_ai_wire::UpdateChatAgentSettingsRequest {
                    mode: stand_in.chat.agent_mode.clone(),
                    fast: stand_in.chat.agent_fast,
                },
            ));
        }
        if let Some(folder) = stand_in.chat.agent_working_folder.clone() {
            self.queue_command(Command::UpdateFolder(
                chat,
                koru_ai_wire::UpdateChatFolderRequest {
                    folder: Some(folder),
                },
            ));
        }
        if stand_in.chat.private {
            self.queue_command(Command::UpdatePrivacy(
                chat,
                koru_ai_wire::UpdateChatPrivacyRequest { private: true },
            ));
        }
        let policy = &stand_in.tool_policy;
        if policy.mode != ToolMode::Manual || !policy.workspace_ids.is_empty() {
            self.queue_command(Command::UpdateToolPolicy(
                chat,
                UpdateChatToolPolicyRequest {
                    mode: policy.mode,
                    workspace_ids: policy.workspace_ids.clone(),
                },
            ));
        }
        true
    }

    /// What a new chat starts on: the default agent where one is set and
    /// can be had (D40), else the provider last chosen — *Auto*.
    fn current_selection(&self) -> Option<ModelSelection> {
        let settings = self.settings.as_ref()?;
        let provider = self
            .default_agent_provider()
            .or_else(|| settings.selected_provider.clone())?;
        let (requested_model, default_reasoning) = match provider.as_str() {
            AiProviderId::OPENAI if settings.openai.credential_configured => {
                (settings.openai.model.clone()?, settings.openai.reasoning)
            }
            AiProviderId::OLLAMA => (settings.ollama.model.clone()?, settings.ollama.reasoning),
            AiProviderId::KORU => (AiProviderId::KORU_HOSTED_MODEL.to_owned(), self.reasoning),
            _ if agents::is_agent_provider(&provider) => {
                (AGENT_DEFAULT_MODEL.to_owned(), ReasoningChoice::Default)
            }
            _ => return None,
        };
        Some(ModelSelection {
            provider,
            requested_model,
            default_reasoning,
        })
    }

    /// **No chat yet, and none on its way from a link**: the chat view is
    /// waiting for a model. Its chip asks for one, send is off, and Enter
    /// opens the picker.
    fn awaiting_model(&self) -> bool {
        self.transcript.is_none() && self.pending_link_chat.is_none()
    }

    /// Open the composer's model picker, as a press on its chip does.
    fn open_model_picker(&mut self) -> bool {
        self.tool_access_open = false;
        self.tool_mode_open = false;
        self.approval_mode_open = false;
        self.chat_model_open = true;
        self.refresh_agent_status();
        self.prefetch_agent_models();
        self.reset_picker();
        true
    }

    /// **Start this surface's chat on `model`**, picked in the composer: a
    /// chat with no message yet takes it, and a surface with no chat yet
    /// shows a new one on it, made only when its first message is sent
    /// ([`Self::begin_unmade_chat`]). A begun agent chat takes another of
    /// its own agent's models, from its next message, where its host takes
    /// one. The pick is remembered as what new chats start on — the choice
    /// the setup used to save — so the next chat opens on it.
    pub(crate) fn start_chat_on(&mut self, model: ModelSelection) -> bool {
        self.web_search_armed = true;
        let sent = match &self.transcript {
            // A chat with no message yet takes it: the service's, or the
            // stand-in of one not made yet, which keeps it here.
            Some(transcript) if transcript.messages.is_empty() => {
                let chat = transcript.chat.id;
                self.send_command(Command::UpdateModel(
                    chat,
                    UpdateChatModelRequest {
                        model: model.clone(),
                    },
                ))
            }
            // A chat that has begun keeps its provider: another model of
            // its agent, from its next message, where the agent's host
            // takes one ([`Self::model_locked`]); nothing else.
            Some(transcript)
                if transcript.chat.current_model.provider == model.provider
                    && agents::is_agent_provider(&model.provider)
                    && !self.model_locked() =>
            {
                let chat = transcript.chat.id;
                self.send_command(Command::UpdateModel(
                    chat,
                    UpdateChatModelRequest {
                        model: model.clone(),
                    },
                ))
            }
            Some(_) => return true,
            None => {
                self.begin_unmade_chat(model.clone());
                true
            }
        };
        self.remember_choice(&model);
        sent
    }

    /// Save `model` as the provider (and, for OpenAI and Ollama, the model)
    /// a new chat starts on. After the chat's own command, so the chat is
    /// made before the settings answer asks for validation again.
    fn remember_choice(&mut self, model: &ModelSelection) {
        let Some(settings) = &self.settings else {
            return;
        };
        let same_provider = settings.selected_provider.as_ref() == Some(&model.provider);
        let model_update = match model.provider.as_str() {
            AiProviderId::OPENAI
                if settings.openai.model.as_deref() != Some(model.requested_model.as_str()) =>
            {
                Some(Command::UpdateOpenAi(UpdateOpenAiSettingsRequest {
                    model: Some(model.requested_model.clone()),
                    // A level belongs to a model; a new model starts on
                    // the provider's default, as a chat's switch does.
                    reasoning: ReasoningChoice::Default,
                }))
            }
            AiProviderId::OLLAMA
                if settings.ollama.model.as_deref() != Some(model.requested_model.as_str()) =>
            {
                Some(Command::UpdateOllama(UpdateOllamaSettingsRequest {
                    endpoint: settings.ollama.endpoint.clone(),
                    model: Some(model.requested_model.clone()),
                    reasoning: ReasoningChoice::Default,
                }))
            }
            _ => None,
        };
        if let Some(command) = model_update {
            self.send_command(command);
        }
        if !same_provider {
            self.send_command(Command::SelectProvider(SelectProviderRequest {
                provider: model.provider.clone(),
            }));
        }
    }

    /// The one line a provider's validation reads as when it is not ready.
    fn validation_line(&self, validation: &ProviderValidationResponse) -> String {
        let koru = validation.provider.as_str() == AiProviderId::KORU;
        match validation.status {
            ProviderValidationStatus::Unconfigured if koru => {
                koru_i18n::t(self.lang, koru_i18n::Key::AiKoruUnconfigured).into()
            }
            ProviderValidationStatus::Unavailable if koru => {
                koru_i18n::t(self.lang, koru_i18n::Key::AiKoruUnreachable).into()
            }
            status => match &validation.failure {
                Some(failure) => failure_text(failure, self.lang),
                None => validation_status_text(status).into(),
            },
        }
    }

    /// Move between authored user turns without making transcript navigation a
    /// text-editing operation. The final downward step returns keyboard focus
    /// to the fixed composer.
    fn navigate_questions(&mut self, down: bool) -> bool {
        self.follow_tail = false;
        let area = self.conversation_rect();
        if self.messages_dirty || (self.message_width - area.w).abs() > 0.5 {
            self.rebuild_messages(area.w);
        }
        let questions = self
            .message_layouts
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .map(|message| message.top)
            .collect::<Vec<_>>();
        let content_extent = self.conversation_content_extent();
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] AI chat question-navigation questions={} current={:?} focus={:?} scroll={:.1}",
            questions.len(),
            self.question_navigation,
            self.focus,
            self.conversation_scroll,
        ));
        if questions.is_empty() {
            self.focus = Focus::Composer;
            self.question_navigation = None;
            self.question_navigation_anchor = None;
            return true;
        }

        let target = if down {
            match self.question_navigation {
                Some(index) if index + 1 < questions.len() => Some(index + 1),
                Some(_) => None,
                None if self.focus == Focus::Composer => None,
                None => questions
                    .iter()
                    .position(|top| *top > self.conversation_scroll + 1.0),
            }
        } else {
            match self.question_navigation {
                Some(index) => index.checked_sub(1).or(Some(0)),
                None if self.focus == Focus::Composer => Some(questions.len() - 1),
                None => questions
                    .iter()
                    .rposition(|top| *top < self.conversation_scroll - 1.0)
                    .or(Some(0)),
            }
        };

        let Some(index) = target else {
            self.focus = Focus::Composer;
            self.question_navigation = None;
            self.question_navigation_anchor = None;
            self.conversation_extent = content_extent;
            self.conversation_scroll = self
                .conversation_scroll
                .min((self.conversation_extent - area.h).max(0.0));
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] AI chat question-navigation target=composer scroll={:.1}",
                self.conversation_scroll,
            ));
            return true;
        };
        let target_top = questions[index];
        self.focus = Focus::None;
        self.response_selection = None;
        self.question_navigation = Some(index);
        self.question_navigation_anchor = Some(target_top);
        self.conversation_extent = content_extent.max(target_top + area.h);
        self.conversation_scroll = target_top;
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] AI chat question-navigation target={} top={target_top:.1}",
            index + 1,
        ));
        true
    }

    fn send_composer(&mut self) -> bool {
        // **A reply is running: the message is queued** ([`queue`]), and
        // sent when the reply completes.
        if self.reply_busy() {
            return self.queue_composer();
        }
        // **No model yet, so nothing to send to**: Enter opens the picker,
        // and the message waits in the composer for the chat it makes.
        if self.awaiting_model() {
            return self.chat_model_open || self.open_model_picker();
        }
        // **A provisional dictation is not sent.** The words are still being
        // heard; the send waits for them to become text.
        if self.composer.composing() {
            return true;
        }
        let message = self.composer.message();
        let typed = message.markdown;
        if typed.trim().is_empty() && !self.has_strip() {
            return false;
        }
        let Some(chat_id) = self.transcript.as_ref().map(|t| t.chat.id) else {
            self.status = Some("The chat is still opening.".into());
            return true;
        };
        let markdown = typed.trim().to_owned();
        if markdown.is_empty() && !self.has_strip() {
            return false;
        }
        let chips: Vec<DocumentId> = message
            .documents
            .iter()
            .map(|(document, _)| *document)
            .collect();
        let (explicit_refs, overflowed) = self.turn_refs(&chips);
        // A new chat is made by its first message, never before: the
        // create is queued ahead of the message.
        if !self.make_unmade_chat() {
            return true;
        }
        if self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
        }) {
            let key = GrantKey {
                chat: chat_id,
                markdown: markdown.clone(),
                documents: explicit_refs
                    .iter()
                    .filter_map(|reference| match reference {
                        ContextRef::Document { document_id } => Some(*document_id),
                        ContextRef::MessageBlock { .. } => None,
                    })
                    .collect(),
                attachments: self.strip().iter().map(|draft| draft.id).collect(),
                current: self.context_workspace,
            };
            if self.grants_ready.as_ref() == Some(&key) {
                self.grants_ready = None;
            } else if key.current.is_some() || !key.documents.is_empty() {
                if self
                    .pending_grants
                    .as_ref()
                    .is_some_and(|(pending, _)| *pending == key)
                {
                    return true;
                }
                let op = uuid::Uuid::new_v4();
                if self.queue_command(Command::AddGrants(
                    chat_id,
                    key.current,
                    key.documents.clone(),
                    op,
                )) {
                    self.pending_grants = Some((key, op));
                }
                return true;
            }
        }
        // Not while an attachment is still being stored, or could not be.
        let Some(sent_attachments) = self.take_attachments_for_send(chat_id) else {
            return true;
        };
        for draft in &sent_attachments {
            self.recall_restages.remove(&draft.id);
        }
        let attachment_ids: Vec<_> = sent_attachments.iter().map(|draft| draft.id).collect();
        let history_attachments: Vec<_> = sent_attachments
            .iter()
            .map(|draft| koru_ai_core::ChatAttachment {
                id: draft.id,
                message_id: None,
                ordinal: None,
                kind: draft.kind,
                source: draft.source,
                file_name: draft.file_name.clone(),
                media_type: String::new(),
                byte_count: draft.byte_count,
                dimensions: draft.size,
                origin: draft.origin.clone(),
                reference_path: draft.reference_path.clone(),
                created_at: koru_ai_core::Timestamp::now(),
            })
            .collect();
        self.usage_report = None;
        // A node link may be a task whose pill caption is its task label.
        // The same document can appear elsewhere in this chat, so only a
        // document-only link may seed the document title cache. Node links
        // use the Store title lookup when no title is known yet.
        let node_documents: HashSet<DocumentId> = koru_link::wrapped(&markdown)
            .into_iter()
            .filter_map(|token| match token.link {
                koru_link::KoruLink::Document {
                    document,
                    node: Some(_),
                } => Some(DocumentId::from_uuid(document)),
                _ => None,
            })
            .collect();
        for (document, title) in &message.documents {
            if !title.trim().is_empty() && !node_documents.contains(document) {
                self.document_titles.insert(*document, title.clone());
            }
        }
        // A chat still called *New chat* takes its name from this, its
        // first message (the review's note): the way ChatGPT names a chat,
        // without a model call — the first line, cut at a word. A document
        // chip is read as its title, as the sent card shows it, never as its
        // `koru://` link.
        let auto_title = self
            .transcript
            .as_ref()
            .filter(|transcript| transcript.chat.title == DEFAULT_CHAT_TITLE)
            .and_then(|_| {
                let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
                let (read, _) = doc_links::titled(&typed, &self.document_titles, untitled);
                title_from_message(&read)
            });
        // A message sent by hand resumes a paused queue: what is queued
        // follows it.
        if !self.sending_queued {
            self.queues.resume(chat_id);
        }
        self.pending_user = Some(markdown.clone());
        self.pulse = Some(PulsingDots::new(Instant::now()));
        self.live_usage = None;
        self.live_sources.clear();
        self.open_searches.clear();
        self.live_reasoning.clear();
        self.reasoning_expanded = self.effective_reasoning() != Some(ReasoningChoice::Off);
        self.pending_assistant_message_id = None;
        self.question_navigation = None;
        self.question_navigation_anchor = None;
        // The next layout pass knows the exact top of this optimistic user
        // card. Move there once; streaming below it must not keep moving the
        // reader after that initial, intentional navigation.
        self.scroll_to_pending_question = true;
        self.pending_question_anchor = None;
        self.draft.clear();
        // Set aside whole rather than cleared: a send that does not go puts
        // the message back as it was, chips and undo included.
        let recalled_from = self.history_cursor.zip(self.history_entries.clone());
        let prior_draft = recalled_from
            .as_ref()
            .and_then(|_| self.history_draft.take());
        let prior_attachments = recalled_from
            .as_ref()
            .and_then(|_| self.history_attachment_draft.take());
        self.sent_message = Some(SentMessage {
            chat: chat_id,
            composer: self.composer.stash(),
            attachments: sent_attachments,
            markdown: markdown.clone(),
            documents: !chips.is_empty() || !message.skills.is_empty(),
            from_queue: self.sending_queued,
            recalled_from,
        });
        self.forget_input_walk();
        if let Some(draft) = prior_draft {
            self.composer.unstash(draft);
        }
        if let Some(drafts) = prior_attachments {
            for draft in drafts {
                self.strips.push(chat_id, draft);
            }
        }
        self.sent_inputs.push(LocalInput {
            chat: chat_id,
            message_id: None,
            markdown: markdown.clone(),
            attachments: history_attachments,
        });
        self.messages_dirty = true;
        let request = SendTurnRequest {
            operation_id: uuid::Uuid::new_v4(),
            chat_id,
            surface: SURFACE,
            markdown,
            explicit_refs,
            web_search: self.web_search_armed && self.web_search_available(),
            // The chip sets the chat's persisted effort (R5); no per-turn
            // override rides the request.
            reasoning_override: None,
            system_prompt_override: self
                .debug_prompt_enabled
                .then(|| self.system_prompt.value())
                .filter(|prompt| !prompt.trim().is_empty()),
            replace_pinned_documents: true,
            attachment_ids,
            answers: None,
            // Each skill chip by name (D121); the text says `$name` for it.
            skills: message.skills.clone(),
        };
        let sent = if self.context_document.is_some() {
            self.staged_embedded_send = Some(request);
            self.durable_observations = 0;
            self.waiting = true;
            true
        } else {
            self.queue_command(Command::Send(request))
        };
        if !sent {
            // The queue would not take it: the message comes back.
            self.give_back_sent(None);
            return true;
        }
        if let Some(title) = auto_title {
            self.send_command(Command::Rename(chat_id, RenameChatRequest { title }));
        }
        // After the send, which clears the status line on success.
        if overflowed {
            self.too_many_documents();
        }
        true
    }

    /// The send did not go: the message comes back to the composer as it
    /// was — text, chips and quotes, and attachments, the one the service named
    /// red — and the reason is said. Something typed since is kept rather
    /// than overwritten.
    fn give_back_sent(&mut self, failure: Option<&koru_ai_wire::Failure>) {
        if let Some(sent) = self.sent_message.as_ref() {
            if let Some(index) = self.sent_inputs.iter().rposition(|input| {
                input.chat == sent.chat
                    && input.message_id.is_none()
                    && input.markdown == sent.markdown
            }) {
                self.sent_inputs.remove(index);
            }
        }
        self.waiting = false;
        self.attempt = None;
        self.pending_assistant_message_id = None;
        self.pending_user = None;
        self.answering = false;
        self.pending_user_tiles.clear();
        self.pulse = None;
        self.messages_dirty = true;
        if let Some(failure) = failure {
            self.status = Some(if Self::refused_for_attachment(failure) {
                attachments::failure_line(failure, self.lang).0
            } else {
                failure.message.clone()
            });
        }
        let Some(sent) = self.sent_message.take() else {
            return;
        };
        if let Some((index, entries)) = &sent.recalled_from
            && self.shown_chat_id() == Some(sent.chat)
        {
            self.history_draft = Some(self.composer.stash());
            self.history_attachment_draft = Some(self.strips.take(sent.chat));
            self.history_entries = Some(entries.clone());
            self.history_cursor = Some(*index);
            self.history_chat = Some(sent.chat);
            for draft in &sent.attachments {
                self.recall_restages.insert(draft.id, sent.chat);
            }
        }
        if sent.from_queue {
            self.requeue_refused(
                sent.chat,
                queue::Queued {
                    composer: sent.composer,
                    attachments: sent.attachments,
                    markdown: sent.markdown,
                    documents: sent.documents,
                },
                failure,
            );
            return;
        }
        if self.shown_chat_id() == Some(sent.chat) {
            if self.composer.is_blank() {
                self.composer.unstash(sent.composer);
            }
        }
        self.restore_drafts(sent.chat, sent.attachments, failure);
    }

    /// The documents a turn carries, as `explicit_refs`.
    ///
    /// This message's chips first, then the document the host is showing.
    /// In the context pane a turn **replaces** the chat's pinned documents
    /// rather than adding to them (`replace_pinned_documents`), so there every
    /// document an earlier message of this chat named by chip is carried again,
    /// newest first: a chip's document stays pinned in both hosts. Never more
    /// than the worker accepts ([`koru_ai_wire::MAX_CONTEXT_REFS`]); the flag
    /// says this message alone asked for more, so the caller can say so.
    fn turn_refs(&self, chips: &[DocumentId]) -> (Vec<ContextRef>, bool) {
        let mut documents: Vec<DocumentId> = Vec::new();
        let push = |documents: &mut Vec<DocumentId>, document: DocumentId| {
            if !documents.contains(&document) {
                documents.push(document);
            }
        };
        for document in chips {
            push(&mut documents, *document);
        }
        if let Some((document, _)) = &self.context_document {
            push(&mut documents, *document);
        }
        let asked = documents.len();
        if let Some(transcript) = &self.transcript {
            for message in transcript
                .messages
                .iter()
                .rev()
                .filter(|message| message.role == ChatRole::User)
            {
                for document in doc_links::documents(&message.markdown) {
                    push(&mut documents, document);
                }
            }
        }
        documents.truncate(koru_ai_wire::MAX_CONTEXT_REFS);
        let refs = documents
            .into_iter()
            .map(|document_id| ContextRef::Document { document_id })
            .collect();
        (refs, asked > koru_ai_wire::MAX_CONTEXT_REFS)
    }

    /// Ask the reader thread, once each, for the titles of documents that
    /// chips in this chat name and nothing here knows yet. The pinned
    /// documents' own titles are taken first; the store is asked only for the
    /// rest, and never from this thread.
    fn request_document_titles(&mut self) {
        let Some(transcript) = &self.transcript else {
            return;
        };
        for pinned in &transcript.pinned_documents {
            if !pinned.display_title.trim().is_empty() {
                self.document_titles
                    .entry(pinned.document_id)
                    .or_insert_with(|| pinned.display_title.clone());
            }
        }
        let wanted: Vec<DocumentId> = transcript
            .messages
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .flat_map(|message| doc_links::documents(&message.markdown))
            .filter(|document| {
                !self.document_titles.contains_key(document)
                    && !self.titles_requested.contains(document)
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Straight to the reader rather than `send_command`: a background
        // lookup must neither clear the status line nor say "busy" on it. A
        // full queue simply asks again on the next rebuild.
        let Some(reader) = &self.reader else {
            return;
        };
        if self.titles_in_flight {
            return;
        }
        if reader.send(Command::DocumentTitles(wanted.clone())).is_ok() {
            self.waiting = true;
            self.titles_in_flight = true;
            self.titles_requested.extend(wanted);
        }
    }

    /// Say that some of this message's documents did not go with it.
    fn too_many_documents(&mut self) {
        let limit = koru_ai_wire::MAX_CONTEXT_REFS.to_string();
        self.status = Some(koru_i18n::fill(
            koru_i18n::t(self.lang, koru_i18n::Key::AiTooManyDocuments),
            &[&limit],
        ));
    }

    fn apply_settings_fields(&mut self, settings: &ProviderSettingsResponse) {
        if let Some(model) = &settings.openai.model {
            self.openai_model.set_text(&mut self.engine, model);
        } else {
            self.openai_model.set_text(&mut self.engine, "");
        }
        self.ollama_endpoint
            .set_text(&mut self.engine, &settings.ollama.endpoint);
        if let Some(model) = &settings.ollama.model {
            self.ollama_model.set_text(&mut self.engine, model);
        }
        self.reasoning = match selected_setup_provider(settings) {
            SetupProvider::OpenAi => settings.openai.reasoning,
            SetupProvider::Ollama => settings.ollama.reasoning,
            SetupProvider::Koru | SetupProvider::Agents => ReasoningChoice::Default,
        };
    }

    fn setup_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        if self.focus == Focus::OllamaModel
            && matches!(key, Key::Named(NamedKey::Enter))
            && !koru_keys::is_chord(mods)
            && self.ollama_pull_action_rect().is_some()
        {
            return self.pull_ollama_model();
        }
        if self.focus == Focus::SystemPrompt {
            return self.system_prompt.key(&mut self.engine, key, text, mods) == Typed::Handled;
        }
        let field = match self.focus {
            Focus::OpenAiKey => Some(&mut self.openai_key),
            Focus::OpenAiModel => Some(&mut self.openai_model),
            Focus::OllamaEndpoint => Some(&mut self.ollama_endpoint),
            Focus::OllamaModel => Some(&mut self.ollama_model),
            _ => None,
        };
        field.is_some_and(|field| field.key(&mut self.engine, key, text, mods) == Typed::Handled)
    }

    fn setup_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if self.setup_open && self.transcript.is_some() && self.setup_close_button().contains(x, y)
        {
            self.setup_open = false;
            self.focus = Focus::Composer;
            return true;
        }
        if self
            .ollama_search_link
            .is_some_and(|rect| rect.contains(x, y))
        {
            // Off this thread, like every web link the chat opens: the shell
            // can take a moment to hand a URL to the browser.
            if !open_web_link("https://ollama.com/search") {
                self.status = Some("Could not open the Ollama model catalog.".into());
            }
            return true;
        }
        if self
            .ollama_pull_action_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            return self.pull_ollama_model();
        }
        if self.model_options_open {
            if let Some(index) = self.model_option_at(x, y)
                && let Some(model) = self.model_options().get(index).cloned()
            {
                match self.setup_provider {
                    SetupProvider::OpenAi => self.openai_model.set_text(&mut self.engine, &model),
                    SetupProvider::Ollama => self.ollama_model.set_text(&mut self.engine, &model),
                    SetupProvider::Koru | SetupProvider::Agents => {}
                }
                self.model_options_open = false;
                self.focus = match self.setup_provider {
                    SetupProvider::OpenAi => Focus::OpenAiModel,
                    SetupProvider::Ollama => Focus::OllamaModel,
                    SetupProvider::Koru | SetupProvider::Agents => Focus::None,
                };
                return true;
            }
            if !self.field_rects()[1].contains(x, y) {
                self.model_options_open = false;
            }
        }
        if self.retry_link.is_some_and(|rect| rect.contains(x, y)) {
            let Some(provider) = self.setup_provider_id() else {
                return true;
            };
            return self.send_command(Command::Validate(provider));
        }
        if self.transcript.is_some() && self.clear_history_button().contains(x, y) {
            self.open_confirmation(DestructiveAction::ClearHistory);
            return true;
        }
        if self.transcript.is_some() && self.clear_usage_button().contains(x, y) {
            self.open_confirmation(DestructiveAction::ClearUsage);
            return true;
        }
        let cards = self.provider_cards();
        if cards[0].contains(x, y) {
            self.setup_provider = SetupProvider::Koru;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = ReasoningChoice::Default;
            self.focus = Focus::None;
            self.status = Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus).into());
            return true;
        }
        if cards[1].contains(x, y) {
            self.setup_provider = SetupProvider::OpenAi;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = self
                .settings
                .as_ref()
                .map_or(ReasoningChoice::Default, |settings| {
                    settings.openai.reasoning
                });
            self.focus = Focus::OpenAiModel;
            return self.save_setup();
        }
        if cards[3].contains(x, y) {
            if self
                .settings
                .as_ref()
                .is_some_and(|settings| !settings.agents.is_empty())
            {
                self.setup_provider = SetupProvider::Agents;
                self.model_options_open = false;
                self.model_option_offset = 0;
                self.reasoning = ReasoningChoice::Default;
                self.focus = Focus::None;
                self.status = None;
                self.refresh_agent_status();
            }
            return true;
        }
        if cards[2].contains(x, y) {
            self.setup_provider = SetupProvider::Ollama;
            self.model_options_open = false;
            self.model_option_offset = 0;
            self.reasoning = self
                .settings
                .as_ref()
                .map_or(ReasoningChoice::Default, |settings| {
                    settings.ollama.reasoning
                });
            self.focus = Focus::OllamaEndpoint;
            return self.save_setup();
        }
        let fields = self.field_rects();
        if self.debug_prompt_enabled
            && let Some(prompt) = self.system_prompt_rect()
            && self.system_prompt.click(&mut self.engine, prompt, x, y)
        {
            self.focus = Focus::SystemPrompt;
            return true;
        }
        match self.setup_provider {
            SetupProvider::OpenAi => {
                if self.openai_credential_clear_button().contains(x, y)
                    && self.openai_credential_configured()
                    && !self.editing_openai_key
                {
                    self.pending_openai_settings = Some((
                        self.settings.as_ref().and_then(|s| s.openai.model.clone()),
                        self.reasoning,
                    ));
                    self.credential_request = Some(OpenAiCredentialRequest::Clear);
                    self.status = Some("Removing the saved key…".into());
                    return true;
                }
                if self.openai_credential_replace_button().contains(x, y)
                    && self.openai_credential_configured()
                    && !self.editing_openai_key
                {
                    self.editing_openai_key = true;
                    self.focus = Focus::OpenAiKey;
                    return true;
                }
                if self.openai_key.click(&mut self.engine, fields[0], x, y) {
                    self.editing_openai_key = true;
                    self.focus = Focus::OpenAiKey;
                    return true;
                }
                if self.openai_model.click(&mut self.engine, fields[1], x, y) {
                    self.focus = Focus::OpenAiModel;
                    self.model_option_offset = 0;
                    self.model_options_open = !self.model_options().is_empty();
                    return true;
                }
            }
            SetupProvider::Ollama => {
                if self
                    .ollama_endpoint
                    .click(&mut self.engine, fields[0], x, y)
                {
                    self.focus = Focus::OllamaEndpoint;
                    return true;
                }
                if self.ollama_model.click(&mut self.engine, fields[1], x, y) {
                    self.focus = Focus::OllamaModel;
                    self.model_option_offset = 0;
                    self.model_options_open = !self.model_options().is_empty();
                    return true;
                }
            }
            SetupProvider::Koru => {}
            SetupProvider::Agents => {
                if self.click_agent_row(x, y) {
                    return true;
                }
            }
        }
        if self.reasoning_button().contains(x, y) && !self.reasoning_choices().is_empty() {
            self.cycle_reasoning();
            return true;
        }
        if self.setup_save_button().contains(x, y) {
            return self.save_setup();
        }
        false
    }

    fn drag_setup_input(&mut self, x: f32) -> bool {
        let fields = self.field_rects();
        self.openai_key.drag(&mut self.engine, fields[0], x)
            || self.openai_model.drag(&mut self.engine, fields[1], x)
            || self.ollama_endpoint.drag(&mut self.engine, fields[0], x)
            || self.ollama_model.drag(&mut self.engine, fields[1], x)
            || self.system_prompt_rect().is_some_and(|rect| {
                self.system_prompt
                    .drag(&mut self.engine, rect, x, self.pointer.1)
            })
    }

    fn save_setup(&mut self) -> bool {
        match self.setup_provider {
            SetupProvider::Agents => {
                let Some(agent) = self.setup_agent.clone() else {
                    self.status =
                        Some(koru_i18n::t(self.lang, koru_i18n::Key::AiAgentPickOne).into());
                    return true;
                };
                if !self
                    .agent_status
                    .get(&agent)
                    .is_some_and(|status| status.selectable())
                {
                    self.status = self.agent_not_ready_line(&agent);
                    self.refresh_agent_status();
                    return true;
                }
                self.status = None;
                self.send_command(Command::SelectProvider(SelectProviderRequest {
                    provider: agent,
                }))
            }
            SetupProvider::Koru => {
                self.status =
                    Some(koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus).into());
                self.send_command(Command::SelectProvider(SelectProviderRequest {
                    provider: AiProviderId::parse(AiProviderId::KORU)
                        .expect("built-in provider id"),
                }))
            }
            SetupProvider::OpenAi => {
                let model = (!self.openai_model.is_empty()).then(|| self.openai_model.value());
                if !self.openai_key.is_empty() {
                    self.pending_openai_settings = Some((model, self.reasoning));
                    self.credential_request =
                        Some(OpenAiCredentialRequest::Save(self.openai_key.value()));
                    self.openai_key.set_text(&mut self.engine, "");
                    self.editing_openai_key = false;
                    self.status = Some("Saving the key securely…".into());
                    return true;
                }
                if self
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.openai.credential_configured)
                {
                    let Some(model) = model else {
                        self.status = Some("Choose an available OpenAI model.".into());
                        self.model_options_open = true;
                        return true;
                    };
                    return self.send_command(Command::UpdateOpenAi(UpdateOpenAiSettingsRequest {
                        model: Some(model),
                        reasoning: self.reasoning,
                    }));
                }
                self.status = Some("Enter an OpenAI project API key.".into());
                true
            }
            SetupProvider::Ollama => {
                let endpoint = self.ollama_endpoint.value();
                let model = self.ollama_model.value();
                self.send_command(Command::UpdateOllama(UpdateOllamaSettingsRequest {
                    endpoint,
                    model: (!model.is_empty()).then_some(model),
                    reasoning: self.reasoning,
                }))
            }
        }
    }

    fn pull_ollama_model(&mut self) -> bool {
        if self.pull_active {
            self.pull_status = Some("Stopping model pull…".into());
            return self.send_command(Command::CancelPull);
        }
        let model = self.ollama_model.value();
        if model.is_empty() {
            return false;
        }
        let handled = self.send_command(Command::Pull(PullOllamaModelRequest {
            model: model.clone(),
        }));
        if self.waiting {
            self.pull_active = true;
            self.pull_status = Some(format!("Starting {model}…"));
        }
        handled
    }

    fn reasoning_choices(&self) -> Vec<ReasoningChoice> {
        let Some(validation) = &self.validation else {
            return Vec::new();
        };
        if self.setup_provider_id().as_ref() != Some(&validation.provider) {
            return Vec::new();
        }
        validation
            .selected_model
            .as_ref()
            .map_or_else(Vec::new, |model| {
                choices_for_reasoning(&model.capabilities.reasoning)
            })
    }

    fn constrain_reasoning(&mut self, validation: &ProviderValidationResponse) {
        let Some(model) = &validation.selected_model else {
            return;
        };
        let choices = choices_for_reasoning(&model.capabilities.reasoning);
        if choices.is_empty() {
            self.reasoning = ReasoningChoice::Default;
        } else if !choices.contains(&self.reasoning) {
            self.reasoning = choices
                .contains(&model.selection.default_reasoning)
                .then_some(model.selection.default_reasoning)
                .unwrap_or(choices[0]);
        }
        if validation.provider.as_str() != AiProviderId::OPENAI || !model.capabilities.web_search {
            self.web_search_armed = false;
        }
    }

    fn cycle_reasoning(&mut self) {
        let choices = self.reasoning_choices();
        if let Some(index) = choices.iter().position(|choice| *choice == self.reasoning) {
            self.reasoning = choices[(index + 1) % choices.len()];
        } else if let Some(first) = choices.first() {
            self.reasoning = *first;
        }
    }

    fn content_rect(&self) -> Rect {
        let width = (self.viewport.w - PAD * 2.0).max(1.0).min(CONTENT_MAX_W);
        Rect::new(
            self.viewport.x + (self.viewport.w - width) * 0.5,
            self.viewport.y,
            width,
            self.viewport.h,
        )
    }

    /// Where the conversation may start: a margin's worth below the top —
    /// the surface has no header row of its own.
    fn chat_top(&self) -> f32 {
        self.viewport.y + PAD
    }

    /// **A chat with nothing in it yet** — no message, none on its way, no
    /// answer streaming — opens on a greeting with the composer centred under
    /// it. A chat still loading from a link is not empty: it has messages
    /// the surface has not read yet.
    fn chat_is_empty(&self) -> bool {
        if self.pending_user.is_some()
            || self.attempt.is_some()
            || self.staged_embedded_send.is_some()
            || self.usage_report.is_some()
        {
            return false;
        }
        match &self.transcript {
            Some(transcript) => transcript.messages.is_empty(),
            None => self.pending_link_chat.is_none(),
        }
    }

    /// The empty chat's greeting, picked at random the first time this chat
    /// shows it and held while it is shown. A chat made for a surface that
    /// was waiting for one keeps the line it was already showing; another
    /// chat gets a new one.
    fn greeting(&mut self) -> Option<koru_i18n::Key> {
        if !self.chat_is_empty() {
            return None;
        }
        let chat = self.transcript.as_ref().map(|t| t.chat.id);
        let index = match self.greeting {
            Some((shown, index)) if shown == chat || shown.is_none() => index,
            _ => (uuid::Uuid::new_v4().as_u128() % GREETINGS.len() as u128) as usize,
        };
        self.greeting = Some((chat, index));
        Some(GREETINGS[index])
    }

    /// The composer's box: its text, one to eight lines, over the toolbar
    /// row, inside the padding. At the bottom of a chat with messages;
    /// centred, under the greeting, in an empty one. Everything on or around
    /// the composer — the send button, the toolbar, the `@` picker's room,
    /// the conversation above, the quiet row below — derives from this.
    fn composer_rect(&mut self) -> Rect {
        let content = self.content_rect();
        let width = content.w.max(MIN_COMPOSER_W);
        let available = (width - COMPOSER_PAD_X * 2.0).max(1.0);
        let max_chip_w = available.min(280.0);
        let mut chip_widths = Vec::new();
        if let Some((_, title)) = &self.context_document {
            chip_widths.push(
                ContextChip {
                    label: title,
                    secondary: "Koru",
                    icon: icons::DOCUMENT,
                    state: ContextState::Ready,
                }
                .width(&mut self.engine, max_chip_w),
            );
        }
        let drafts = self.strip().to_vec();
        for draft in &drafts {
            let secondary = self.attachment_chip_secondary(draft);
            let label = draft.chip_label();
            chip_widths.push(
                ContextChip {
                    label: &label,
                    secondary: &secondary,
                    icon: self.attachment_chip_icon(draft),
                    state: self.attachment_chip_state(draft),
                }
                .width(&mut self.engine, max_chip_w),
            );
        }
        self.chip_band = composer_band::ChipBand::layout(
            &chip_widths,
            available,
            ContextChip {
                label: "",
                secondary: "",
                icon: icons::DOCUMENT,
                state: ContextState::Ready,
            }
            .height(),
            self.chip_scroll,
        );
        self.chip_scroll = self.chip_band.scroll;
        // One to eight visual lines of the composer's own text, then it
        // scrolls — measured by the editor at this width.
        let text_h = self
            .composer
            .text_height(
                (width - COMPOSER_PAD_X * 2.0).max(1.0),
                COMPOSER_MIN_LINES,
                COMPOSER_MAX_LINES,
            )
            .ceil();
        let height = self.strip_band()
            + COMPOSER_PAD_TOP
            + text_h
            + COMPOSER_TEXT_GAP
            + COMPOSER_TOOLBAR_H
            + COMPOSER_PAD_BOTTOM;
        // The quiet row under the box, when there is one, is part of the
        // block: the box stands on it rather than on the window's edge.
        let below = self.composer_footer_h();
        let bottom = self.viewport.y + self.viewport.h - COMPOSER_BOTTOM_MARGIN - below - height;
        let y = if self.chat_is_empty() {
            // The greeting, the composer and its row as one block, centred;
            // never lower than where the composer sits once the chat has begun.
            let above = role::title().metrics.line_height + GREETING_GAP + COMPOSER_GAP;
            (self.viewport.y + (self.viewport.h - above - height - below) * 0.5 + above).min(bottom)
        } else {
            bottom
        };
        let rect = Rect::new(content.x, y, width, height);
        self.composer_hit_rect = rect;
        let mode_name = (self.send_mode() != queue::SendMode::Stop)
            .then(|| self.approval_presentation())
            .flatten()
            .map(|(row, _)| row.title);
        self.composer_shell = Some(composer_shell::layout(
            rect,
            self.strip_band(),
            text_h,
            koru_i18n::t(self.lang, koru_i18n::Key::AiComposerAddContext),
            self.send_button_name(),
            mode_name.as_deref(),
            self.dictation
                .offered
                .then(|| koru_i18n::t(self.lang, koru_i18n::Key::AiDictate)),
            self.dictation
                .microphone
                .offered
                .then(|| koru_i18n::t(self.lang, koru_i18n::Key::AiMicrophoneChoose)),
        ));
        if below > 0.0 {
            // The folder control is as wide as what it shows.
            let label = self.folder_label_shown();
            let icon = self
                .engine
                .icon_layout(icons::FOLDER, ActionButton::ICON_SIZE)
                .width();
            let text = self.engine.simple_layout(&label, role::meta()).width();
            self.folder_button_w = ActionButton::PAD_X * 2.0 + icon + ActionButton::GAP + text;
        }
        // The picker may float anywhere above the composer's text, below
        // the header: over the conversation, never over the toolbar.
        let text = self.composer_text_rect();
        let top = self.chat_top();
        let panels = Rect::new(
            self.viewport.x,
            top,
            self.viewport.w,
            (text.y + text.h - top).max(1.0),
        );
        self.composer.place(text, panels);
        rect
    }

    /// The composer's text box, padding included: everything above the
    /// toolbar row. The editor insets its words by the same padding
    /// ([`composer`]).
    fn composer_text_rect(&self) -> Rect {
        if let Some(shell) = &self.composer_shell {
            return shell.text();
        }
        // Below the strip of attachments, when the message has any.
        let band = self.strip_band();
        Rect::new(
            self.composer_hit_rect.x,
            self.composer_hit_rect.y + band,
            self.composer_hit_rect.w,
            (self.composer_hit_rect.h - band - COMPOSER_TOOLBAR_H - COMPOSER_PAD_BOTTOM).max(1.0),
        )
    }

    fn conversation_rect(&mut self) -> Rect {
        let composer = self.composer_rect();
        let content = self.content_rect();
        let footer_h = self
            .footer_layouts(content.w)
            .map_or(0.0, |(words, _)| words.height() + 5.0);
        let top = self.chat_top();
        let rect = Rect::new(
            content.x,
            top,
            content.w,
            (composer.y - COMPOSER_GAP - self.queue_band_h() - footer_h - top).max(1.0),
        );
        self.conversation_hit_rect = rect;
        rect
    }

    /// The transcript uses the same overlay scrollbar geometry as documents.
    /// Its vertical band follows the centred conversation, while its thumb
    /// sits at the host viewport's right edge like the document scrollbar.
    fn conversation_scrollbar(&self) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
        if self.showing_setup() {
            return None;
        }
        let conversation = self.conversation_hit_rect;
        let band = Rect::new(
            self.viewport.x,
            conversation.y,
            self.viewport.w,
            conversation.h,
        );
        koru_ui::scrollbar::ScrollbarGeom::for_content(
            band,
            self.conversation_extent,
            self.conversation_scroll,
        )
    }

    /// The send (and stop) button, in the composer's bottom-right corner,
    /// inset by the composer's own padding like the toolbar beside it.
    fn send_button(&self) -> Rect {
        if let Some(shell) = &self.composer_shell {
            return shell.send();
        }
        let composer = self.composer_hit_rect;
        let side = BUTTON_H;
        Rect::new(
            composer.x + composer.w - COMPOSER_PAD_X - side,
            composer.y + composer.h - COMPOSER_PAD_BOTTOM - side,
            side,
            side,
        )
    }

    /// The centered content column used by the settings/setup surface.
    ///
    /// Matches the chat conversation's maximum width so the settings page does
    /// not sprawl on a wide tab or wide pane.
    fn setup_content_rect(&self) -> Rect {
        let w = (self.viewport.w - PAD * 2.0).max(1.0).min(CONTENT_MAX_W);
        let x = self.viewport.x + (self.viewport.w - w) * 0.5;
        Rect::new(x, self.viewport.y, w, self.viewport.h)
    }

    fn provider_cards(&self) -> [Rect; 4] {
        let content = self.setup_content_rect();
        let y = self.viewport.y + 76.0;
        [0, 1, 2, 3].map(|i| Rect::new(content.x, y + i as f32 * 56.0, content.w, 48.0))
    }

    fn field_rects(&self) -> [Rect; 2] {
        let content = self.setup_content_rect();
        let y = self.viewport.y + SETUP_FIELDS_Y;
        [
            Rect::new(content.x, y, content.w, FIELD_H),
            Rect::new(content.x, y + FIELD_H + 10.0, content.w, FIELD_H),
        ]
    }

    fn openai_credential_configured(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.openai.credential_configured)
    }

    fn setup_text_field_at(&self, x: f32, y: f32) -> bool {
        let fields = self.field_rects();
        match self.setup_provider {
            SetupProvider::OpenAi => {
                fields[1].contains(x, y)
                    || ((!self.openai_credential_configured() || self.editing_openai_key)
                        && fields[0].contains(x, y))
            }
            SetupProvider::Ollama => {
                fields[0].contains(x, y)
                    || (fields[1].contains(x, y)
                        && !self
                            .ollama_pull_action_rect()
                            .is_some_and(|rect| rect.contains(x, y)))
            }
            SetupProvider::Koru | SetupProvider::Agents => false,
        }
    }

    fn openai_credential_replace_button(&self) -> Rect {
        let field = self.field_rects()[0];
        Rect::new(field.x + field.w - 104.0, field.y, 62.0, field.h)
    }

    fn openai_credential_clear_button(&self) -> Rect {
        let field = self.field_rects()[0];
        Rect::new(field.x + field.w - 42.0, field.y, 42.0, field.h)
    }

    fn model_options(&self) -> Vec<String> {
        let mut models: Vec<_> = match self.setup_provider {
            SetupProvider::OpenAi => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .cloned()
                .collect(),
            SetupProvider::Ollama => self.ollama_models.clone(),
            SetupProvider::Koru | SetupProvider::Agents => Vec::new(),
        };
        sort_model_options(&mut models);
        models.dedup();
        models
    }

    fn chat_model_options(&self) -> Vec<String> {
        let Some(provider) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.current_model.provider.as_str())
        else {
            return Vec::new();
        };
        let mut models: Vec<_> = match provider {
            AiProviderId::OPENAI => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .cloned()
                .collect(),
            AiProviderId::OLLAMA => self.ollama_models.clone(),
            _ => Vec::new(),
        };
        sort_model_options(&mut models);
        models.dedup();
        models
    }

    fn change_chat_model(&mut self, requested_model: String) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        let mut model = transcript.chat.current_model.clone();
        model.requested_model = requested_model;
        self.web_search_armed = true;
        // A reasoning level belongs to a model's capability set. Carrying a
        // level from the previous model can make an otherwise valid switch
        // fail, so a model-only choice returns to the provider default.
        model.default_reasoning = ReasoningChoice::Default;
        self.send_command(Command::UpdateModel(
            transcript.chat.id,
            UpdateChatModelRequest { model },
        ))
    }

    fn chat_model_button(&self) -> Rect {
        let toolbar = self.composer_toolbar_rect();
        // As wide as its label wants, up to half the toolbar; the mode
        // control gives way to its left.
        let width = (self.chip_label_w + 44.0)
            .max(78.0)
            .min((toolbar.w * 0.5).max(78.0))
            .min(toolbar.w);
        Rect::new(toolbar.x + toolbar.w - width, toolbar.y, width, toolbar.h)
    }

    /// Whether the composer has its quiet row below: an agent chat's folder
    /// to show. Before the first message it chooses the folder; afterwards a
    /// chosen folder stays named, and a chat left in the home folder, the
    /// default, shows nothing — and the row takes no room.
    fn shows_composer_footer(&self) -> bool {
        false
    }

    /// The height the quiet row takes under the composer, its gap included:
    /// nothing when there is no row.
    fn composer_footer_h(&self) -> f32 {
        if self.shows_composer_footer() {
            COMPOSER_FOOTER_GAP + COMPOSER_FOOTER_H
        } else {
            0.0
        }
    }

    /// **The quiet row under the composer box**, as wide as the box and
    /// left-aligned with it. Derived from the box ([`Self::composer_rect`]),
    /// which leaves room for it, so drawing, hit-testing and the hover
    /// cursor all read the same place.
    #[cfg(test)]
    fn composer_footer_rect(&self) -> Option<Rect> {
        if !self.shows_composer_footer() {
            return None;
        }
        let composer = self.composer_hit_rect;
        Some(Rect::new(
            composer.x,
            composer.y + composer.h + COMPOSER_FOOTER_GAP,
            composer.w,
            COMPOSER_FOOTER_H,
        ))
    }

    /// An agent chat's folder, in the quiet row under the composer — a
    /// folder icon and its name, outside the box like a checkout under a
    /// prompt (it sat in the toolbar until then). Before the first message
    /// it chooses the folder; the agent starts in it, so the choice closes
    /// with that message, and from then on a chosen folder stays named here,
    /// unpressable. A chat left in the home folder, the default, shows
    /// nothing here once it has begun.
    #[cfg(test)]
    fn folder_button(&self) -> Option<Rect> {
        let row = self.composer_footer_rect()?;
        Some(Rect::new(
            row.x,
            row.y,
            self.folder_button_w.max(60.0).min(row.w),
            row.h,
        ))
    }

    /// [`folder_label`](Self::folder_label) cut with an ellipsis to fit the
    /// composer's width beside the folder icon, so a long folder name never
    /// runs past the box it sits under.
    fn folder_label_shown(&mut self) -> String {
        let label = self.folder_label();
        let icon = self
            .engine
            .icon_layout(icons::FOLDER, ActionButton::ICON_SIZE)
            .width();
        let room = self.composer_hit_rect.w - ActionButton::PAD_X * 2.0 - icon - ActionButton::GAP;
        self.engine.elided(&label, role::meta(), room)
    }

    fn folder_label(&self) -> String {
        let folder = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_working_folder.as_deref());
        match folder {
            Some(folder) => std::path::Path::new(folder)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| folder.to_owned()),
            None => koru_i18n::t(self.lang, koru_i18n::Key::AiAgentFolderHome).to_owned(),
        }
    }

    /// The Manual/Automatic chip, left of the model chip. **Not on an agent
    /// chat**: the policy is enforced only by Koru's own tool loop, and an
    /// agent asks through its own choices, so there the chip would do
    /// nothing.
    fn tool_mode_button(&self) -> Option<Rect> {
        if self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
        }) {
            return None;
        }
        let toolbar = self.composer_toolbar_rect();
        let model = self.chat_model_button();
        let width = (toolbar.w * 0.24).clamp(72.0, 105.0);
        Some(Rect::new(
            (model.x - 6.0 - width).max(toolbar.x),
            toolbar.y,
            width.min((model.x - toolbar.x - 6.0).max(1.0)),
            toolbar.h,
        ))
    }

    fn koru_chat(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            transcript.chat.current_model.provider.as_str() == AiProviderId::KORU
        })
    }

    fn reasoning_toggle_rect(&self) -> Option<Rect> {
        if self.live_reasoning.is_empty() {
            return None;
        }
        let content = self.content_rect();
        let composer = self.composer_hit_rect;
        Some(Rect::new(
            content.x,
            composer.y - COMPOSER_GAP - 22.0,
            content.w.min(220.0),
            20.0,
        ))
    }

    fn draw_reasoning_drawer(&mut self, canvas: &mut Scene, root: Affine, composer: Rect) {
        if self.live_reasoning.is_empty() {
            return;
        }
        let p = theme::p();
        let content = self.content_rect();
        let title = if self.attempt.is_some() && self.draft.is_empty() {
            koru_i18n::t(self.lang, koru_i18n::Key::AiThinking)
        } else {
            koru_i18n::t(self.lang, koru_i18n::Key::AiReasoning)
        };
        let label = self.engine.simple_layout(&title, role::meta());
        let y = composer.y - COMPOSER_GAP - label.height();
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            content.x,
            y,
            Some(if self.reasoning_expanded {
                p.accent
            } else {
                p.text_dim
            }),
        );
        if self.reasoning_expanded {
            let preview: String = self.live_reasoning.chars().take(400).collect();
            let body = self
                .engine
                .simple_layout_wrapped(&preview, role::meta(), content.w);
            scene::draw_layout(
                canvas,
                root,
                &body,
                content.x,
                y - body.height() - 4.0,
                Some(p.text_dim),
            );
        }
    }

    fn web_search_available(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            let model = &transcript.chat.current_model;
            model.provider.as_str() == AiProviderId::OPENAI
                && (model.requested_model.starts_with("gpt-5")
                    || model.requested_model.starts_with("gpt-4.1"))
        })
    }

    /// **The private mark**, the toolbar's first control: whether agents
    /// and connected assistants may read this chat. It sat left of the
    /// title; with the title gone it is the only door to the setting, so it
    /// moved here, where both hosts show it.
    /// The private mark at the toolbar's left — `None` while it is hidden
    /// for now (`koru_features::PRIVATE_CHAT_BUTTON_SHOWN`), and then every
    /// control laid out after it starts at the toolbar's left instead, and
    /// nothing draws, hits or names it.
    fn privacy_button(&self) -> Option<Rect> {
        if !koru_features::private_chat_button() {
            return None;
        }
        self.transcript.as_ref()?;
        let toolbar = self.composer_toolbar_rect();
        Some(Rect::new(
            toolbar.x + toolbar.h + 6.0,
            toolbar.y,
            toolbar.h,
            toolbar.h,
        ))
    }

    fn web_search_button(&self) -> Option<Rect> {
        None
    }

    /// The composer's toolbar row: inside its padding, left of the send
    /// button.
    fn composer_toolbar_rect(&self) -> Rect {
        let composer = self.composer_hit_rect;
        // Left of the fixed microphone. Its chooser reserves the lane after
        // it, so revealing the chevron moves neither the model nor send.
        let send = self.mic_button().unwrap_or_else(|| self.send_button());
        let x = composer.x + COMPOSER_PAD_X;
        Rect::new(
            x,
            composer.y + composer.h - COMPOSER_PAD_BOTTOM - COMPOSER_TOOLBAR_H,
            (send.x - x - 6.0).max(1.0),
            COMPOSER_TOOLBAR_H,
        )
    }

    fn plus_button(&self) -> Rect {
        if let Some(shell) = &self.composer_shell {
            return shell.plus();
        }
        let toolbar = self.composer_toolbar_rect();
        Rect::new(toolbar.x, toolbar.y, toolbar.h, toolbar.h)
    }

    fn tool_access_button(&self) -> Rect {
        let toolbar = self.composer_toolbar_rect();
        // The chip to its right: the mode chip — Manual/Automatic, or an
        // agent's approval mode — or the model chip where there is neither.
        let next_x = self
            .tool_mode_button()
            .or_else(|| self.approval_mode_button())
            .unwrap_or_else(|| self.chat_model_button())
            .x;
        let x = self
            .web_search_button()
            .or_else(|| self.privacy_button())
            .map_or(toolbar.x + toolbar.h + 6.0, |icon| icon.x + icon.w + 6.0);
        // **It hugs its label** (D107): its padding, the label and the
        // caret, as the approval-mode chip does — never a wider button
        // with the label centred in it — up to its widest, and never into
        // the chip after it. Until it is first measured, its widest.
        let content = if self.tool_access_chip_w > 0.0 {
            self.tool_access_chip_w
        } else {
            TOOL_ACCESS_CHIP_MAX_W
        };
        let width = content
            .min(TOOL_ACCESS_CHIP_MAX_W)
            .min((next_x - x - 6.0).max(1.0));
        Rect::new(x, toolbar.y, width, toolbar.h)
    }

    /// The workspace-access chip's label as drawn in `chip`: whole when it
    /// fits, elided to the room its padding and caret leave when it does
    /// not.
    #[cfg(test)]
    fn tool_access_label_shown(&mut self, label: &str, chip: Rect) -> String {
        if self.tool_access_chip_w <= chip.w + 0.01 {
            return label.to_owned();
        }
        let caret = self
            .engine
            .icon_layout(icons::CARET_DOWN, ActionButton::ICON_SIZE)
            .width();
        let room = (chip.w - ActionButton::PAD_X * 2.0 - ActionButton::GAP - caret).max(1.0);
        self.engine.elided(label, role::row(), room)
    }

    /// The chat's model's effort control: the one the service sent with
    /// the transcript, or — for an agent model, and a new chat not made yet
    /// — the levels its agent's listing carries (agent-models D14).
    fn effort_control(&self) -> Option<&ReasoningControl> {
        let transcript = self.transcript.as_ref()?;
        transcript
            .reasoning_control
            .as_ref()
            .or_else(|| self.agent_model_control(&transcript.chat.current_model))
            .filter(|control| !effort::stops(control).is_empty())
    }

    /// What the chat's effort setting means under its model's control.
    fn effective_reasoning(&self) -> Option<ReasoningChoice> {
        let transcript = self.transcript.as_ref()?;
        let control = self.effort_control()?;
        effort::resolve(control, transcript.chat.current_model.default_reasoning)
    }

    /// Whether the chat is an agent chat that has begun: from its first
    /// message it keeps its agent (AGENTUX D1-2).
    fn agent_chat_begun(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
                && !transcript.messages.is_empty()
        })
    }

    /// **A begun agent chat keeps its agent; its model may move to another
    /// of that agent's, from its next message**, as its level does — where
    /// its agent's listing says the session host takes a model with each
    /// message ([`Self::models_per_turn`]). Everywhere else the model is the
    /// one the chat started on: the agent started on it and a live session
    /// is not moved, so the picker's other rows and its *1M context* pill
    /// only show it. A chat with no message yet chooses freely.
    fn model_locked(&self) -> bool {
        self.agent_chat_begun()
            && self.transcript.as_ref().is_some_and(|transcript| {
                !self
                    .models_per_turn
                    .contains(&transcript.chat.current_model.provider)
            })
    }

    /// **A begun agent chat's level changes from its next message**
    /// (koru-ai D70), as Fast does — where its agent's listing says the
    /// session host takes a level per message (protocol v20). Against an
    /// older host the level stays the one the chat started at (S5), and the
    /// picker's effort strip only shows it: a level such a host would
    /// ignore is never sent.
    fn effort_locked(&self) -> bool {
        self.agent_chat_begun()
            && self.transcript.as_ref().is_some_and(|transcript| {
                !self
                    .levels_per_turn
                    .contains(&transcript.chat.current_model.provider)
            })
    }

    /// Set the chat's effort: the persisted default the next turn uses.
    fn choose_effort(&mut self, choice: ReasoningChoice) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        if self.effective_reasoning() == Some(choice) {
            return true;
        }
        let mut model = transcript.chat.current_model.clone();
        model.default_reasoning = choice;
        self.send_command(Command::UpdateModel(
            transcript.chat.id,
            UpdateChatModelRequest { model },
        ))
    }

    /// The chip's text: the model, then its effort when the model has one.
    /// An agent chat names its level only when one was chosen (S2): on
    /// *Default* the agent decides, and the chip says nothing for it.
    pub(crate) fn chip_label(&self) -> String {
        let Some(transcript) = &self.transcript else {
            // No chat yet: the chip asks for the model that will make one.
            return koru_i18n::t(self.lang, koru_i18n::Key::AiChooseModel).into();
        };
        let model = &transcript.chat.current_model;
        let name = if model.provider.as_str() == AiProviderId::KORU {
            AiProviderId::KORU_HOSTED_MODEL_LABEL.to_owned()
        } else if agents::is_agent_provider(&model.provider) {
            let agents = self
                .settings
                .as_ref()
                .map(|settings| settings.agents.as_slice())
                .unwrap_or_default();
            agent_chip_label(
                agents,
                &model.provider,
                &model.requested_model,
                self.listed_agent_models(&model.provider),
            )
        } else {
            model.requested_model.clone()
        };
        let chosen = !agents::is_agent_provider(&model.provider)
            || model.default_reasoning != ReasoningChoice::Default;
        match self.effective_reasoning() {
            Some(choice) if chosen => format!("{name} · {}", effort::word(self.lang, choice)),
            _ => name,
        }
    }

    #[cfg(test)]
    fn tool_access_label(&self) -> String {
        use koru_i18n::Key;
        let Some(transcript) = &self.transcript else {
            return koru_i18n::t(self.lang, Key::AiToolWorkspaceAccess).into();
        };
        match self.granted_workspace_count(&transcript.tool_policy.workspace_ids) {
            1 => koru_i18n::t(self.lang, Key::AiToolSpaceOne).into(),
            count => koru_i18n::fill(
                koru_i18n::t(self.lang, Key::AiToolSpaces),
                &[&count.to_string()],
            ),
        }
    }

    fn tool_mode_label(&self) -> &'static str {
        use koru_i18n::Key;
        let key = match &self.transcript {
            Some(transcript) if transcript.tool_policy.mode == ToolMode::Auto => {
                Key::AiToolModeAutomatic
            }
            _ => Key::AiToolModeManual,
        };
        koru_i18n::t(self.lang, key)
    }

    fn model_options_panel(&self) -> Option<Rect> {
        let count = self.model_option_count().min(MODEL_VISIBLE_ROWS);
        if !self.model_options_open || count == 0 {
            return None;
        }
        let field = self.field_rects()[1];
        Some(Rect::new(
            field.x,
            field.y + field.h + 4.0,
            field.w,
            count as f32 * 30.0 + 8.0,
        ))
    }

    fn model_option_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = self.model_options_panel()?;
        if !panel.contains(x, y) {
            return None;
        }
        let visible = ((y - panel.y - 4.0) / 30.0).floor().max(0.0) as usize;
        let index = self.model_option_offset + visible;
        let row = Rect::new(
            panel.x + 4.0,
            panel.y + 4.0 + visible as f32 * 30.0,
            panel.w - 8.0,
            30.0,
        );
        (visible < MODEL_VISIBLE_ROWS && index < self.model_option_count() && row.contains(x, y))
            .then_some(index)
    }

    fn model_option_count(&self) -> usize {
        match self.setup_provider {
            SetupProvider::OpenAi => self
                .openai_models
                .iter()
                .filter(|model| likely_openai_text_model(model))
                .count(),
            SetupProvider::Ollama => self.ollama_models.len(),
            SetupProvider::Koru | SetupProvider::Agents => 0,
        }
    }

    fn validation_retryable(&self) -> bool {
        self.validation.as_ref().is_some_and(|validation| {
            self.setup_provider_id().as_ref() == Some(&validation.provider)
                && (matches!(
                    validation.status,
                    ProviderValidationStatus::Unavailable | ProviderValidationStatus::Failed
                ) || (self.setup_provider == SetupProvider::Ollama
                    && validation.status == ProviderValidationStatus::NoModels)
                    || validation
                        .failure
                        .as_ref()
                        .is_some_and(|failure| failure.retryable))
        })
    }

    fn setup_save_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x + content.w - 108.0,
            self.viewport.y
                + SETUP_FIELDS_Y
                + 134.0
                + self.ollama_catalog_gap()
                + self.agents_gap(),
            108.0,
            BUTTON_H,
        )
    }

    fn ollama_pull_action_rect(&self) -> Option<Rect> {
        if self.setup_provider != SetupProvider::Ollama {
            return None;
        }
        let model = self.ollama_model.value();
        if model.trim().is_empty() || self.ollama_models.iter().any(|known| known == &model) {
            return None;
        }
        let field = self.field_rects()[1];
        Some(Rect::new(field.x + field.w - 66.0, field.y, 66.0, field.h))
    }

    fn reasoning_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x,
            self.viewport.y + SETUP_FIELDS_Y + 86.0 + self.ollama_catalog_gap(),
            content.w,
            BUTTON_H,
        )
    }

    fn ollama_catalog_gap(&self) -> f32 {
        if self.setup_provider == SetupProvider::Ollama && self.ollama_models.is_empty() {
            26.0
        } else {
            0.0
        }
    }

    fn setup_close_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x + content.w - 70.0,
            self.viewport.y + 10.0,
            70.0,
            BUTTON_H,
        )
    }

    fn clear_history_button(&self) -> Rect {
        let content = self.setup_content_rect();
        Rect::new(
            content.x,
            self.viewport.y + self.viewport.h - PAD - BUTTON_H,
            118.0,
            BUTTON_H,
        )
    }

    fn clear_usage_button(&self) -> Rect {
        Rect::new(
            self.clear_history_button().x + self.clear_history_button().w + 8.0,
            self.viewport.y + self.viewport.h - PAD - BUTTON_H,
            118.0,
            BUTTON_H,
        )
    }

    fn system_prompt_rect(&self) -> Option<Rect> {
        if !self.debug_prompt_enabled {
            return None;
        }
        let y = self.viewport.y + SETUP_FIELDS_Y + 242.0 + self.agents_gap();
        let bottom = if self.transcript.is_some() {
            self.clear_history_button().y - 10.0
        } else {
            self.viewport.y + self.viewport.h - PAD
        };
        let height = (bottom - y).min(110.0);
        (height >= 52.0).then(|| {
            let content = self.setup_content_rect();
            Rect::new(content.x, y, content.w, height)
        })
    }

    /// Stable identity for ordinary controls whose hover state is drawn from
    /// `pointer`. Repaint only when the pointer crosses between controls, not
    /// for every pixel of movement across the surface.
    fn control_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        if self.showing_setup() {
            if self.setup_open && self.transcript.is_some() {
                if self.setup_close_button().contains(x, y) {
                    return Some((10, 0));
                }
            }
            if let Some(index) = self
                .provider_cards()
                .iter()
                .position(|rect| rect.contains(x, y))
            {
                return Some((1, index));
            }
            if self.setup_provider == SetupProvider::Agents
                && let Some(index) = self.agent_row_at(x, y)
            {
                return Some((15, index));
            }
            if self.setup_save_button().contains(x, y) {
                return Some((2, 0));
            }
            if !self.reasoning_choices().is_empty() && self.reasoning_button().contains(x, y) {
                return Some((2, 1));
            }
            if self.transcript.is_some() {
                if self.clear_history_button().contains(x, y) {
                    return Some((2, 3));
                }
                if self.clear_usage_button().contains(x, y) {
                    return Some((2, 4));
                }
            }
            if self.openai_credential_configured() && !self.editing_openai_key {
                if self.openai_credential_replace_button().contains(x, y) {
                    return Some((2, 5));
                }
                if self.openai_credential_clear_button().contains(x, y) {
                    return Some((2, 6));
                }
            }
            if self
                .ollama_pull_action_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return Some((3, 0));
            }
            if self.model_options_open
                && let Some(index) = self.model_option_at(x, y)
            {
                return Some((4, index));
            }
            None
        } else {
            if self
                .jump_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((17, 0));
            }
            if self
                .stored_open_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return Some((18, 0));
            }
            if self
                .privacy_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 8));
            }
            if self.chat_model_button().contains(x, y) {
                return Some((5, 5));
            }
            if self
                .web_search_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 7));
            }
            if self
                .reasoning_toggle_rect()
                .is_some_and(|rect| rect.contains(x, y))
            {
                return Some((5, 11));
            }
            if self
                .tool_mode_button()
                .is_some_and(|button| button.contains(x, y))
            {
                return Some((5, 9));
            }
            if let Some(part) = self.approval_mode_hover_at(x, y) {
                return Some(part);
            }
            if let Some(part) = self.microphone_hover_at(x, y) {
                return Some(part);
            }
            if let Some(part) = self.skill_hover_at(x, y) {
                return Some(part);
            }
            if self
                .mic_chooser_button()
                .is_some_and(|chevron| chevron.contains(x, y))
            {
                return Some((5, 14));
            }
            if self.tool_mode_open {
                if let Some(modes) = self.tool_mode_buttons() {
                    for (index, rect) in modes.into_iter().enumerate() {
                        if rect.contains(x, y) {
                            return Some((13, index));
                        }
                    }
                }
            }
            if self.tool_access_open {
                if let Some(workspace) = self.tool_access_workspace_at(x, y)
                    && let Some(index) = self
                        .destinations
                        .iter()
                        .position(|candidate| candidate.workspace_id == workspace)
                {
                    return Some((14, index));
                }
            }
            // The send button keeps its identity, 4, from when the header's
            // buttons were 1 to 3.
            if self.send_button().contains(x, y) {
                return Some((5, 4));
            }
            if self.destination_open.is_some()
                && let Some(index) = self.destination_at(x, y)
            {
                return Some((8, index));
            }
            None
        }
    }

    fn activate_send_button(&mut self) -> bool {
        if self.reply_busy() {
            // Queue while the composer holds something, Stop while it is
            // empty. Pointer and keyboard use this same route.
            match (self.send_mode(), self.attempt) {
                (queue::SendMode::Stop, Some(attempt)) => {
                    return self.send_command(Command::Cancel(attempt));
                }
                _ => {
                    self.queue_composer();
                    return true;
                }
            }
        }
        if !self.awaiting_model() {
            self.send_composer();
        }
        true
    }

    fn paint_composer_box(&self, canvas: &mut Scene, root: Affine, rect: Rect) {
        let p = theme::p();
        scene::floating_panel(
            canvas,
            root,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            &scene::Panel::popover(18.0).border(if self.focus == Focus::Composer {
                p.focus_ring
            } else {
                p.panel_border
            }),
        );
    }

    fn paint_composer_text(&mut self, canvas: &mut Scene, root: Affine) {
        let focused = self.focus == Focus::Composer;
        self.composer.set_focused(focused);
        self.composer.draw(
            canvas,
            root,
            focused,
            self.pointer,
            koru_i18n::t(
                self.lang,
                if self.transcript.as_ref().is_some_and(|chat| {
                    agents::is_agent_provider(&chat.chat.current_model.provider)
                }) {
                    koru_i18n::Key::AiComposerAgentPlaceholder
                } else {
                    koru_i18n::Key::AiComposerPlaceholder
                },
            ),
        );
    }

    /// Paint the controls whose model and microphone widths depend on the
    /// current chat. The plus and send have their own element targets.
    fn paint_composer_toolbar_content(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        composer: Rect,
        model_button: Rect,
        mode_button: Option<Rect>,
    ) -> bool {
        if self.draw_dictation_row(canvas, root) {
            self.draw_reasoning_drawer(canvas, root, composer);
            return true;
        }
        if let Some(mode_button) = mode_button {
            ActionButton::new(
                self.tool_mode_label(),
                ActionStyle::Ghost,
                Some(icons::CARET_DOWN),
            )
            .draw(
                canvas,
                root,
                &mut self.engine,
                mode_button,
                mode_button.contains(self.pointer.0, self.pointer.1),
            );
        }
        if let Some(privacy) = self.privacy_button() {
            let private = self
                .transcript
                .as_ref()
                .is_some_and(|transcript| transcript.chat.private);
            IconButton::new(icons::DETECTIVE, privacy)
                .with_state(IconButtonState {
                    hovered: privacy.contains(self.pointer.0, self.pointer.1),
                    active: private,
                    disabled: false,
                })
                .draw(canvas, root, &mut self.engine);
        }
        if let Some(search) = self.web_search_button() {
            IconButton::new(icons::GLOBE, search)
                .with_state(IconButtonState {
                    hovered: search.contains(self.pointer.0, self.pointer.1),
                    active: self.web_search_armed,
                    disabled: false,
                })
                .draw(canvas, root, &mut self.engine);
        }
        self.draw_reasoning_drawer(canvas, root, composer);
        let koru_chat = self.koru_chat();
        let model_label = self.chip_label();
        let model_caret =
            (!koru_chat || self.effort_control().is_some()).then_some(icons::CARET_DOWN);
        let caret_width = model_caret.map_or(0.0, |caret| {
            ActionButton::GAP
                + self
                    .engine
                    .icon_layout(caret, ActionButton::ICON_SIZE)
                    .width()
        });
        let shown_label = self.engine.elided(
            &model_label,
            role::row(),
            (model_button.w - ActionButton::PAD_X * 2.0 - caret_width).max(0.0),
        );
        let hovered = model_button.contains(self.pointer.0, self.pointer.1);
        self.model_chevron.set(self.chat_model_open, Instant::now());
        if model_caret.is_some() {
            if hovered {
                scene::fill(
                    canvas,
                    root,
                    theme::p().hover_tint,
                    &scene::rrect(
                        model_button.x,
                        model_button.y,
                        model_button.w,
                        model_button.h,
                        ActionButton::RADIUS,
                    ),
                );
            }
            let text_w = self.engine.simple_layout(&shown_label, role::row()).width();
            ActionButton::new(&shown_label, ActionStyle::Ghost, None).draw(
                canvas,
                root,
                &mut self.engine,
                Rect::new(
                    model_button.x,
                    model_button.y,
                    model_button.w - caret_width,
                    model_button.h,
                ),
                false,
            );
            let content_x = model_button.x + (model_button.w - text_w - caret_width) * 0.5;
            let caret_rect = Rect::new(
                content_x + text_w + ActionButton::GAP,
                model_button.y,
                caret_width - ActionButton::GAP,
                model_button.h,
            );
            paint_turning_caret(
                canvas,
                root,
                &mut self.engine,
                caret_rect,
                ActionButton::ICON_SIZE,
                self.model_chevron.progress(),
                theme::p().text,
            );
        } else {
            ActionButton::new(&shown_label, ActionStyle::Ghost, None).draw(
                canvas,
                root,
                &mut self.engine,
                model_button,
                hovered,
            );
        }
        false
    }

    fn paint_composer_plus(&mut self, canvas: &mut Scene, root: Affine, plus: Rect) {
        IconButton::new(icons::PLUS, plus)
            .with_state(IconButtonState {
                hovered: plus.contains(self.pointer.0, self.pointer.1),
                active: self.plus_menu.is_some(),
                disabled: false,
            })
            .draw(canvas, root, &mut self.engine);
    }

    fn paint_composer_send(&mut self, canvas: &mut Scene, root: Affine, send: Rect) {
        let mode = self.send_mode();
        let enabled = match mode {
            queue::SendMode::Stop => true,
            queue::SendMode::Queue => self.composer_has_content(),
            queue::SendMode::Send => !self.awaiting_model() && self.composer_has_content(),
        };
        draw_composer_action(
            canvas,
            root,
            &mut self.engine,
            send,
            mode,
            enabled,
            send.contains(self.pointer.0, self.pointer.1),
        );
    }

    fn paint_composer_split(&mut self, canvas: &mut Scene, root: Affine, rect: Rect) {
        let Some((row, tone)) = self.approval_presentation() else {
            return;
        };
        let mode = self.send_mode();
        let enabled = match mode {
            queue::SendMode::Stop => true,
            queue::SendMode::Queue => self.composer_has_content(),
            queue::SendMode::Send => !self.awaiting_model() && self.composer_has_content(),
        };
        let mut button = SplitButton {
            rect,
            tone,
            action_enabled: enabled,
            action_hot: false,
            action_focused: self.focus == Focus::SendArrow,
            menu_hot: false,
            menu_focused: self.focus == Focus::ModeChevron,
            menu_open: self.approval_mode_open,
            reveal: self.send_chevron_reveal.progress(),
            chevron_turn: self.send_chevron.progress(),
        };
        button.action_hot = button
            .action_rect()
            .contains(self.pointer.0, self.pointer.1);
        button.menu_hot = button.menu_rect().contains(self.pointer.0, self.pointer.1);
        let now = Instant::now();
        self.send_chevron.set(self.approval_mode_open, now);
        self.send_chevron_reveal.set(
            button.menu_hot || button.menu_focused || button.menu_open,
            now,
        );
        button.reveal = self.send_chevron_reveal.progress();
        button.chevron_turn = self.send_chevron.progress();
        button.draw(
            canvas,
            root,
            &mut self.engine,
            if mode == queue::SendMode::Queue {
                icons::CLOCK
            } else {
                icons::ARROW_UP
            },
            row.icon,
        );
    }

    fn draw_chat(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        // **No title row.** A chat's name is its tab's in a workspace and
        // its row's in the Chats list; the surface itself starts with the
        // conversation.
        self.draw_messages(canvas, root);
        // Over the transcript, under everything that floats over it.
        self.draw_add_pill(canvas, root);
        let composer = self.composer_rect();
        self.draw_greeting(canvas, root);
        self.draw_queue(canvas, root);
        self.stored_open_link = None;
        let content = self.content_rect();
        if let Some((layout, open)) = self.footer_layouts(content.w) {
            let y = composer.y - COMPOSER_GAP - self.queue_band_h() - layout.height();
            scene::draw_layout(canvas, root, &layout, content.x, y, Some(p.text_dim));
            // *Open* after a stored reply (D78): a link in the accent, as
            // the chat's other links are, lit under the pointer.
            if let Some((open, dx, dy)) = open {
                let (x, top) = (content.x + dx, y + dy);
                let hit = Rect::new(x - 3.0, top - 2.0, open.width() + 6.0, open.height() + 4.0);
                if hit.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                    );
                }
                scene::draw_layout_first_line(canvas, root, &open, x, top, Some(p.accent));
                self.stored_open_link = Some(hit);
            }
        }
        // The shell frame resolves the box, band, editor and toolbar from
        // current state. Its ancestry stays open while each mutable host part
        // paints, so F12 sees the editor and every control under KoruComposer.
        let chip_label = self.chip_label();
        self.chip_label_w = self.engine.simple_layout(&chip_label, role::row()).width();
        let model_button = self.chat_model_button();
        let mode_button = self.tool_mode_button();
        let microphone_choice_shown = self.mic_chooser_button().is_some();
        let shell = self.composer_shell.take().expect("composer was laid out");
        let mut recording = false;
        shell
            .frame
            .paint_host(&shell.tree, canvas, root, |node, canvas, root| {
                match node.name {
                    "ComposerBox" => self.paint_composer_box(canvas, root, node.rect),
                    "ComposerContextBand" => self.draw_strip(canvas, root, node.rect),
                    "ComposerText" => self.paint_composer_text(canvas, root),
                    "ComposerToolbar" => {
                        recording = self.paint_composer_toolbar_content(
                            canvas,
                            root,
                            composer,
                            model_button,
                            mode_button,
                        );
                    }
                    "ComposerMicrophoneChoice" if !recording => {
                        self.draw_mic_chooser(canvas, root, node.rect, microphone_choice_shown);
                    }
                    "ComposerMicrophone" if !recording => self.draw_mic(canvas, root, node.rect),
                    "ComposerAddContext" if !recording => {
                        self.paint_composer_plus(canvas, root, node.rect);
                    }
                    "ComposerSplitButton" if !recording => {
                        self.paint_composer_split(canvas, root, node.rect);
                    }
                    "ComposerSend" if !recording && shell.mode().is_none() => {
                        self.paint_composer_send(canvas, root, node.rect);
                    }
                    _ => {}
                }
            });
        self.composer_shell = Some(shell);
        // A link's card (D95): over the transcript and the composer, under
        // the pickers and panes that float over both.
        self.link_card.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            self.pointer,
            Instant::now(),
        );
        self.draw_model_picker(canvas, root);
        self.draw_tool_mode_picker(canvas, root);
        self.draw_approval_mode_picker(canvas, root);
        self.draw_tool_access_picker(canvas, root);
        self.draw_microphone_panel(canvas, root);
        self.web_search_tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
        self.picker_tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
        self.queue_tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
        self.draw_dictation_tooltip(canvas, root);
        self.composer.draw_panels(canvas, root);
        self.draw_skill_list(canvas, root);
    }

    /// An empty chat's greeting: the Koru mark and question at the same
    /// title size, centered together above the composer.
    fn draw_greeting(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(key) = self.greeting() else {
            return;
        };
        let area = self.conversation_rect();
        let title = role::title();
        let logo_size = title.metrics.size;
        let gap = theme::Space::S2.px(theme::Profile::Desktop);
        let text_max = (area.w - logo_size - gap).max(1.0);
        let greeting = koru_i18n::t(self.lang, key);
        let natural = self.engine.simple_layout(greeting, title);
        let text_width = natural.width().min(text_max);
        let layout = if natural.width() <= text_max {
            natural
        } else {
            self.engine.simple_layout_wrapped(greeting, title, text_max)
        };
        let x = area.x + (area.w - logo_size - gap - text_width).max(0.0) * 0.5;
        let y = (area.y + area.h - GREETING_GAP - layout.height()).max(area.y);
        let logo_y = y + (layout.height() - logo_size) * 0.5;
        let (logo_x, text_x) = if self.lang.is_rtl() {
            (x + text_width + gap, x)
        } else {
            (x, x + logo_size + gap)
        };
        KoruLogo::new(logo_size).draw(canvas, root, logo_x, logo_y);
        scene::draw_layout(canvas, root, &layout, text_x, y, Some(theme::p().text));
    }

    /// **Where an agent chat moved to another of its agent's models**: the
    /// first reply on each model after another, with the words its note in
    /// front of it reads — *Switched to Sonnet 4.6*. Read from the model
    /// each reply recorded when it was sent, so it needs nothing kept beside
    /// the messages; a reply that recorded none is passed over. Empty for
    /// every other chat.
    fn model_switches(&self) -> Vec<(MessageId, String)> {
        let Some(transcript) = &self.transcript else {
            return Vec::new();
        };
        let agent = &transcript.chat.current_model.provider;
        if !agents::is_agent_provider(agent) {
            return Vec::new();
        }
        let mut last: Option<&str> = None;
        let mut switches = Vec::new();
        for message in koru_ai_core::reading_order(&transcript.messages) {
            if message.role != ChatRole::Assistant || message.provider.as_ref() != Some(agent) {
                continue;
            }
            let Some(model) = message.requested_model.as_deref() else {
                continue;
            };
            if last.is_some_and(|last| last != model) {
                let name = self.agent_model_name(agent, model);
                switches.push((
                    message.id,
                    koru_i18n::fill(
                        koru_i18n::t(self.lang, koru_i18n::Key::AiModelSwitchedTo),
                        &[&name],
                    ),
                ));
            }
            last = Some(model);
        }
        switches
    }

    fn rebuild_messages(&mut self, width: f32) {
        let started = Instant::now();
        self.settle_question_cards(false);
        self.tool_activity_layouts.clear();
        self.work_layouts.clear();
        self.notice_layouts.clear();
        let switches = self.model_switches();
        self.request_document_titles();
        let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
        let lang = self.lang;
        let now_ms = koru_ai_core::Timestamp::now().millis();
        let work_width = (width - PAD * 2.0).max(1.0);
        let detail = |activity: &ToolActivity| {
            if is_agent_row(activity) {
                agent_row_body(activity, lang, now_ms)
            } else {
                tool_activity_detail(activity)
            }
        };
        // An opened call's detail is laid out once and kept until what it
        // shows changes: a rebuild runs on every streamed delta.
        let opened_calls = &self.opened_calls;
        self.call_details.retain(|id, _| opened_calls.contains(id));
        let expanded = &self.expanded_outputs;
        let cache = &mut self.call_details;
        // A picture a call read is drawn from its file (D38): its path read
        // as a changed file's is, and the picture asked of the image cache.
        let folder = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_working_folder.clone());
        let images = &self.images;
        let mut wanted_pictures = Vec::new();
        let mut call_detail = |engine: &mut TextEngine, activity: &ToolActivity, width: f32| {
            let pieces = if is_agent_row(activity) {
                tool_output::picture::settle(
                    tool_output::pieces(activity, lang, now_ms, expanded.contains(&activity.id)),
                    |path| {
                        tool_output::open_path::resolve(
                            path,
                            folder.as_deref(),
                            std::env::home_dir().as_deref(),
                        )
                    },
                    |key| images.get(key).map(ImageLoad::progress),
                    &mut wanted_pictures,
                )
            } else {
                tool_activity_detail(activity)
                    .lines()
                    .map(|line| tool_output::Piece::Meta(line.to_owned()))
                    .collect()
            };
            let key = tool_output::layout::fingerprint(&pieces, width);
            if let Some((cached, lines)) = cache.get(&activity.id)
                && *cached == key
            {
                return Rc::clone(lines);
            }
            let lines = Rc::new(tool_output::layout::lay_out(engine, &pieces, width));
            cache.insert(activity.id, (key, Rc::clone(&lines)));
            lines
        };
        let opened = work_view::Opened {
            replies: &self.opened_work,
            groups: &self.opened_step_groups,
            calls: &self.opened_calls,
        };
        let live_offsets = &self.live_offsets;
        let offset = |activity: &ToolActivity| effective_offset(activity, live_offsets);
        let running = self.pending_assistant_message_id;
        let mut inputs = Vec::new();
        // Where the running reply's answer starts in its draft, once a
        // call has cut what came before it away.
        let mut pending_answer: Option<usize> = None;
        if let Some(transcript) = &self.transcript {
            let usage_by_message = transcript
                .usage
                .iter()
                .map(|entry| (entry.message_id, &entry.usage))
                .collect::<HashMap<_, _>>();
            // Where an agent chat moved to another of its agent's models, a
            // grey line in front of the first reply on it, as a notice's.
            for (message, text) in switches {
                self.notice_layouts.push(NoticeLayout {
                    message,
                    top: 0.0,
                    layout: self
                        .engine
                        .simple_layout_wrapped(&text, role::meta(), work_width),
                });
            }
            // An agent's notices (D69): one grey line each, 12 px, wrapped
            // to the column, in front of the reply they came with.
            for notice in &transcript.notices {
                self.notice_layouts.push(NoticeLayout {
                    message: notice.assistant_message_id,
                    top: 0.0,
                    layout: self.engine.simple_layout_wrapped(
                        &notice.text,
                        role::meta(),
                        work_width,
                    ),
                });
            }
            let mut worked: HashSet<MessageId> = HashSet::new();
            // As a person reads them: a message steered into a reply
            // under its question, before the reply (D99).
            for message in koru_ai_core::reading_order(&transcript.messages) {
                if !message.markdown.is_empty() || message.state != MessageState::Pending {
                    let wrap_width = message_wrap_width(message.role, width);
                    let usage_text = (message.role == ChatRole::Assistant
                        && message.state == MessageState::Complete)
                        .then(|| usage_by_message.get(&message.id))
                        .flatten()
                        .and_then(|usage| usage_line(usage, self.lang));
                    let display = if message.markdown.is_empty()
                        && message.state == MessageState::Failed
                    {
                        failure_label(message.error_class, self.lang)
                    } else if message.markdown.is_empty()
                        && message.state == MessageState::Interrupted
                    {
                        koru_i18n::t(self.lang, koru_i18n::Key::AiResponseInterrupted).to_owned()
                    } else {
                        message.markdown.clone()
                    };
                    // The person's own message carries its chips as
                    // `[[koru://doc/…]]`; they read as titled links.
                    let (mut display, links) = if message.role == ChatRole::User {
                        doc_links::titled(
                            &quote_pills::quote_bars(&display),
                            &self.document_titles,
                            untitled,
                        )
                    } else {
                        (display, Vec::new())
                    };
                    let mut blocks: Vec<(MessageBlockId, String)> = transcript
                        .blocks
                        .iter()
                        .filter(|block| block.message_id == message.id)
                        .map(|block| (block.id, block.markdown.clone()))
                        .collect();
                    // A reply that used tools: what the agent said before
                    // its last call folds with the steps; the rest is the
                    // answer (`tool_groups`).
                    let rows = reply_rows(transcript, message.id);
                    if message.role == ChatRole::Assistant
                        && let Some(cut) =
                            tool_groups::cut_reply(&message.markdown, &rows, offset, &blocks)
                    {
                        if cut.answer_start > 0 {
                            display = message.markdown[cut.answer_start..].trim_start().to_owned();
                            blocks = cut.blocks;
                        }
                        worked.insert(message.id);
                        self.work_layouts.push(work_view::build(
                            &mut self.engine,
                            lang,
                            work_width,
                            work_view::WorkInput {
                                message: message.id,
                                live: message.state == MessageState::Pending,
                                started_ms: message.created_at.millis(),
                                worked_ms: tool_groups::worked_ms(
                                    Some(message.created_at.millis()),
                                    message.completed_at.map(|at| at.millis()),
                                    &rows,
                                ),
                                items: cut.items,
                                groups: cut.groups,
                            },
                            &opened,
                            &mut call_detail,
                        ));
                    }
                    inputs.push(MessageInput {
                        id: message.id,
                        role: message.role,
                        state: message.state,
                        markdown: message.markdown.clone(),
                        display,
                        links,
                        wrap_width,
                        usage_text,
                        sources: transcript
                            .sources
                            .iter()
                            .find(|entry| entry.message_id == message.id)
                            .map(|entry| entry.sources.clone())
                            .unwrap_or_default(),
                        blocks,
                        attachments: transcript
                            .attachments
                            .iter()
                            .filter(|attachment| attachment.message_id == Some(message.id))
                            .map(attachments::Tile::of)
                            .collect(),
                    });
                }
            }
            // The running reply: its draft is cut the same way, at the
            // offsets its rows carry or the surface noted as they came.
            if (self.pending_user.is_some() || self.answering)
                && let Some(message) = running
                && !worked.contains(&message)
            {
                let rows = reply_rows(transcript, message);
                if let Some(cut) =
                    tool_groups::cut_reply::<MessageBlockId>(&self.draft, &rows, offset, &[])
                {
                    let answer = &self.draft[cut.answer_start..];
                    pending_answer = Some(self.draft.len() - answer.trim_start().len());
                    worked.insert(message);
                    self.work_layouts.push(work_view::build(
                        &mut self.engine,
                        lang,
                        work_width,
                        work_view::WorkInput {
                            message,
                            live: true,
                            started_ms: self.live_turn_started_ms.unwrap_or(now_ms),
                            worked_ms: 0,
                            items: cut.items,
                            groups: cut.groups,
                        },
                        &opened,
                        &mut call_detail,
                    ));
                }
            }
            // Steps whose reply is not a message here (yet): a view of
            // their own, with no text to cut.
            let mut orphans: Vec<MessageId> = Vec::new();
            for activity in &transcript.tool_activities {
                if !worked.contains(&activity.assistant_message_id)
                    && !orphans.contains(&activity.assistant_message_id)
                {
                    orphans.push(activity.assistant_message_id);
                }
            }
            for message in orphans {
                let rows = reply_rows(transcript, message);
                if let Some(cut) = tool_groups::cut_reply::<MessageBlockId>("", &rows, offset, &[])
                {
                    // Done only once the reply is: a running reply whose
                    // calls have all ended is still working.
                    let live = Some(message) == running
                        || rows.iter().any(|row| !row.status.is_terminal());
                    let started_ms = rows
                        .iter()
                        .map(|row| row.proposed_at.millis())
                        .min()
                        .unwrap_or(now_ms);
                    self.work_layouts.push(work_view::build(
                        &mut self.engine,
                        lang,
                        work_width,
                        work_view::WorkInput {
                            message,
                            live,
                            started_ms,
                            worked_ms: tool_groups::worked_ms(None, None, &rows),
                            items: cut.items,
                            groups: cut.groups,
                        },
                        &opened,
                        &mut call_detail,
                    ));
                }
            }
            // A pending permission request is a card of its own, open, in
            // front of its reply; so is an agent's set of questions (D108),
            // waiting or closed.
            for activity in transcript.tool_activities.iter().filter(|activity| {
                tool_groups::is_card(activity) || questions::is_question_row(activity)
            }) {
                if questions::is_question_row(activity) {
                    let expanded_height = questions::card_height(
                        &mut self.engine,
                        &mut self.question_cards,
                        lang,
                        activity,
                        width,
                    );
                    self.tool_activity_layouts.push(ToolActivityLayout {
                        activity: activity.clone(),
                        before_message: Some(activity.assistant_message_id),
                        top: 0.0,
                        title: self.engine.simple_layout("", role::row()),
                        detail: self.engine.simple_layout("", role::meta()),
                        expanded_height,
                    });
                    continue;
                }
                let title = if is_agent_row(activity) {
                    agent_row_title(activity, self.lang)
                } else {
                    tool_activity_title(activity)
                };
                let detail = self.engine.simple_layout_wrapped(
                    &detail(activity),
                    role::meta(),
                    (width - 28.0).max(1.0),
                );
                let choices = agents::offered_choice_count(activity);
                let actions_height = if choices > 0 {
                    choices as f32 * (BUTTON_H + 6.0) + 6.0
                } else {
                    BUTTON_H + 12.0
                };
                let expanded_height =
                    (43.0 + detail.height() + actions_height).max(TOOL_ACTIVITY_MIN_EXPANDED_H);
                self.tool_activity_layouts.push(ToolActivityLayout {
                    activity: activity.clone(),
                    before_message: Some(activity.assistant_message_id),
                    top: 0.0,
                    title: self.engine.simple_layout_wrapped(
                        &title,
                        role::row(),
                        (width - 46.0).max(1.0),
                    ),
                    detail,
                    expanded_height,
                });
            }
        }
        let mut old = std::mem::take(&mut self.message_layouts);
        // The running reply's last layout, whose closed blocks it keeps.
        let old_live = running
            .filter(|id| !inputs.iter().any(|input| input.id == *id))
            .and_then(|id| {
                old.iter().position(|message| {
                    message.id == Some(id) && message.state == MessageState::Pending
                })
            })
            .map(|index| old.swap_remove(index));
        let (mut messages, stats) =
            layout_cache::reconcile(&mut self.engine, old, inputs, render_chat_block);
        self.last_rebuild_stats = stats;
        if let Some(report) = &self.usage_report {
            let markdown = format_usage_report(report);
            messages.push(MessageLayout {
                id: None,
                top: 0.0,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                display: markdown.clone(),
                links: Vec::new(),
                wrap_width: (width - PAD * 2.0).max(1.0),
                layout: chat_text_layout(&mut self.engine, &markdown, (width - PAD * 2.0).max(1.0)),
                usage_text: None,
                usage: None,
                sources: None,
                markdown,
                blocks: Vec::new(),
                attachments: Vec::new(),
            });
        }
        if self.reply_pending() {
            // Answers sent as the next message have no card of the
            // person's: they are read on the set's card (D108).
            if let Some(markdown) = self.pending_user.clone() {
                let tiles = self.pending_user_tiles.clone();
                let card = self.user_card(&markdown, tiles, width);
                messages.push(card);
            }
            // Messages the running reply took read under the question,
            // in the order sent, and the reply follows them — as the
            // transcript reads once it is read back (D99).
            for steered in self.steered_shown() {
                let card = self.user_card(&steered, Vec::new(), width);
                messages.push(card);
            }
            let assistant_width = message_wrap_width(ChatRole::Assistant, width);
            let usage_text = (!self.draft.is_empty())
                .then_some(self.live_usage.as_ref())
                .flatten()
                .and_then(|usage| usage_line(usage, self.lang));
            // Once the agent has called a tool, what it said before the
            // call is in the work view above; the card shows what follows.
            let start = pending_answer.unwrap_or(0);
            let answer = &self.draft[start..];
            let display = if self.draft.is_empty() && !self.open_searches.is_empty() {
                "Searching the web…".to_owned()
            } else {
                answer.to_owned()
            };
            // Its closed blocks as Markdown, laid out once each; only the
            // open block at the end is laid out again, as plain text.
            let live = running.and_then(|id| {
                let blocks = self.live_answer.blocks(id, start, answer);
                let tail = self.live_answer.tail_id();
                let (mut laid, live_stats) = layout_cache::reconcile(
                    &mut self.engine,
                    old_live.into_iter().collect(),
                    vec![MessageInput {
                        id,
                        role: ChatRole::Assistant,
                        state: MessageState::Pending,
                        markdown: self.draft.clone(),
                        display: display.clone(),
                        links: Vec::new(),
                        wrap_width: assistant_width,
                        usage_text: usage_text.clone(),
                        sources: self.live_sources.clone(),
                        blocks,
                        attachments: Vec::new(),
                    }],
                    |engine, block, markdown, width| {
                        if block == tail {
                            live_markdown::plain_block(engine, block, markdown, width)
                        } else {
                            render_chat_block(engine, block, markdown, width)
                        }
                    },
                );
                self.last_rebuild_stats += live_stats;
                laid.pop()
            });
            // Under the running reply's id, so its work is placed above it.
            messages.push(live.unwrap_or_else(|| MessageLayout {
                id: running,
                top: 0.0,
                role: ChatRole::Assistant,
                state: MessageState::Pending,
                markdown: self.draft.clone(),
                links: Vec::new(),
                wrap_width: assistant_width,
                layout: chat_text_layout(&mut self.engine, &display, assistant_width),
                display,
                usage: usage_text.as_ref().map(|line| {
                    self.engine
                        .simple_layout_wrapped(line, role::hint(), assistant_width)
                }),
                usage_text,
                sources: sources::layout_sources(
                    &mut self.engine,
                    &self.live_sources,
                    assistant_width,
                ),
                blocks: Vec::new(),
                attachments: Vec::new(),
            }));
        }
        self.message_layouts = messages;
        self.position_conversation_items();
        self.message_width = width;
        self.messages_dirty = false;
        self.response_selection = None;
        let image_urls = self
            .message_layouts
            .iter()
            .flat_map(|message| message.blocks.iter())
            .filter_map(|block| block.image.as_ref().map(|image| image.url.clone()))
            .collect::<Vec<_>>();
        for url in image_urls {
            // A reply's picture from a file goes through the call pictures'
            // loader (D38): one cache, one worker, one decoder, one limit.
            match tool_output::picture::reply_picture_path(&url) {
                Some(path) if !is_web_url(&url) => self.ensure_disk_image(path),
                _ => self.ensure_remote_image(&url),
            }
        }
        for path in wanted_pictures {
            self.ensure_disk_image(path);
        }
        tracing::debug!(
            target: "koru_ai::ui",
            surface = "context_pane",
            message_count = self.message_layouts.len(),
            block_count = self
                .message_layouts
                .iter()
                .map(|message| message.blocks.len())
                .sum::<usize>(),
            messages_reused = self.last_rebuild_stats.messages_reused,
            messages_laid_out = self.last_rebuild_stats.messages_laid_out,
            blocks_reused = self.last_rebuild_stats.blocks_reused,
            blocks_laid_out = self.last_rebuild_stats.blocks_laid_out,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "AI Markdown layout rebuilt"
        );
    }

    /// A message of the person's not yet read back from the service — the
    /// one just sent, or one steered into the running reply — laid out
    /// exactly as the durable card will be.
    fn user_card(
        &mut self,
        markdown: &str,
        tiles: Vec<attachments::Tile>,
        width: f32,
    ) -> MessageLayout {
        let untitled = koru_i18n::t(self.lang, koru_i18n::Key::AiDocumentLinkUntitled);
        let user_width = message_wrap_width(ChatRole::User, width);
        let (display, links) = doc_links::titled(
            &quote_pills::quote_bars(markdown),
            &self.document_titles,
            untitled,
        );
        MessageLayout {
            id: None,
            top: 0.0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: markdown.to_owned(),
            layout: layout_cache::display_layout(&mut self.engine, &display, &links, user_width),
            display,
            links,
            wrap_width: user_width,
            usage_text: None,
            usage: None,
            sources: None,
            blocks: Vec::new(),
            attachments: tiles,
        }
    }

    /// Place each reply's work and its pending permission cards in front of
    /// the reply. An inference attempt creates its assistant message before
    /// it proposes tools, so the activity belongs immediately before that
    /// response, not in a second list at the end of the transcript.
    fn position_conversation_items(&mut self) -> f32 {
        let mut top = 0.0;
        let mut placed_work = vec![false; self.work_layouts.len()];
        let mut placed_cards = vec![false; self.tool_activity_layouts.len()];
        // A notice whose reply is not (yet) a message here follows them.
        for notice in &mut self.notice_layouts {
            notice.top = f32::NAN;
        }
        // The NEW divider's band, in front of the first unseen message and
        // its work (D84).
        let divider_at = self.new_divider_index();
        if let Some(divider) = &mut self.new_divider {
            divider.top = None;
        }
        for message_index in 0..self.message_layouts.len() {
            if Some(message_index) == divider_at
                && let Some(divider) = &mut self.new_divider
            {
                divider.top = Some(top);
                top += new_divider::NEW_DIVIDER_H + CARD_GAP;
            }
            if let Some(message_id) = self.message_layouts[message_index].id {
                top = self.place_turn(message_id, top, &mut placed_work, &mut placed_cards);
            }
            self.message_layouts[message_index].top = top;
            top += message_height(&self.message_layouts[message_index]) + CARD_GAP;
        }
        // Work whose reply is not (yet) a message in the list — an attempt's
        // first call before its assistant card lands — follows the messages.
        for index in 0..self.work_layouts.len() {
            if !placed_work[index] {
                let message = self.work_layouts[index].message;
                top = self.place_turn(message, top, &mut placed_work, &mut placed_cards);
            }
        }
        for (index, card) in self.tool_activity_layouts.iter_mut().enumerate() {
            if !placed_cards[index] {
                card.top = top;
                top += card.expanded_height + 6.0;
            }
        }
        for notice in &mut self.notice_layouts {
            if notice.top.is_nan() {
                notice.top = top;
                top += notice.layout.height() + NOTICE_GAP;
            }
        }
        top
    }

    /// A reply's work view, then its cards, from `top`; where they end.
    fn place_turn(
        &mut self,
        message: MessageId,
        mut top: f32,
        placed_work: &mut [bool],
        placed_cards: &mut [bool],
    ) -> f32 {
        // What the agent said beside the reply, first: Devin's notice comes
        // as the session starts, before anything the reply does.
        for notice in &mut self.notice_layouts {
            if notice.top.is_nan() && notice.message == message {
                notice.top = top;
                top += notice.layout.height() + NOTICE_GAP;
            }
        }
        for (index, work) in self.work_layouts.iter_mut().enumerate() {
            if !placed_work[index] && work.message == message {
                placed_work[index] = true;
                work.top = top;
                top += work.height + 6.0;
            }
        }
        for (index, card) in self.tool_activity_layouts.iter_mut().enumerate() {
            if !placed_cards[index] && card.before_message == Some(message) {
                placed_cards[index] = true;
                card.top = top;
                top += card.expanded_height + 6.0;
            }
        }
        top
    }

    fn conversation_content_extent(&self) -> f32 {
        let messages = self
            .message_layouts
            .iter()
            .map(|message| message.top + message_height(message) + CARD_GAP);
        let cards = self
            .tool_activity_layouts
            .iter()
            .map(|card| card.top + card.expanded_height + 6.0);
        let work = self
            .work_layouts
            .iter()
            .map(|work| work.top + work.height + 6.0);
        let notices = self
            .notice_layouts
            .iter()
            .map(|notice| notice.top + notice.layout.height() + NOTICE_GAP);
        messages
            .chain(cards)
            .chain(work)
            .chain(notices)
            .fold(0.0, f32::max)
    }

    fn draw_messages(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.conversation_rect();
        if self.messages_dirty || (self.message_width - area.w).abs() > 0.5 {
            self.rebuild_messages(area.w);
            let content_extent = self.conversation_content_extent();
            let mut anchored_question = false;
            if self.scroll_to_pending_question
                && self.pending_user.is_some()
                && let Some(index) = self
                    .message_layouts
                    .iter()
                    .rposition(|message| message.role == ChatRole::User && message.id.is_none())
            {
                let top = self.message_layouts[index].top;
                self.pending_question_anchor = Some(top);
                self.conversation_scroll = top;
                self.scroll_to_pending_question = false;
                anchored_question = true;
            }
            if self.pending_user.is_none() {
                self.scroll_to_pending_question = false;
            }
            // Keep enough virtual tail for the newest question to remain at
            // the top, including after its optimistic turn becomes durable.
            // The next send replaces this anchor; opening another chat clears
            // it. Removing it on completion clamps short answers to the
            // natural bottom and produces a distracting second scroll.
            self.conversation_extent = self
                .pending_question_anchor
                .into_iter()
                .chain(self.question_navigation_anchor)
                .fold(content_extent, |extent, top| extent.max(top + area.h));
            // Streaming grows the document below the current reading
            // position. Never pull the reader to the new tail; only clamp a
            // position that became invalid because content got shorter.
            if anchored_question {
                // A new question is anchored at the top: following ends.
                self.follow_tail = false;
            }
            if self.scroll_loaded_chat_to_bottom {
                self.conversation_scroll = (self.conversation_extent - area.h).max(0.0);
                self.scroll_loaded_chat_to_bottom = false;
            } else if self.follow_tail {
                // Jumped to the newest message (D77): a streaming reply
                // keeps it there.
                self.conversation_scroll = (self.conversation_extent - area.h).max(0.0);
            } else if !anchored_question {
                self.conversation_scroll = self
                    .conversation_scroll
                    .min((self.conversation_extent - area.h).max(0.0));
            }
            // **An unread chat opens on its NEW divider** (D84), not at its
            // end: once, the first layout that finds the message.
            if let Some(scroll) = self.take_new_divider_scroll() {
                self.follow_tail = false;
                self.scroll_loaded_chat_to_bottom = false;
                self.conversation_scroll = scroll.min((self.conversation_extent - area.h).max(0.0));
            }
            // **A chat coming back opens where it was left** (D102): Back or
            // Forward to it, or a host reopening it, wins over both above.
            if let Some(scroll) = self.take_restored_scroll() {
                self.follow_tail = false;
                self.scroll_loaded_chat_to_bottom = false;
                self.conversation_scroll = scroll.min((self.conversation_extent - area.h).max(0.0));
            }
            if let Some((target, _)) = self.highlight
                && let Some(index) = self
                    .message_layouts
                    .iter()
                    .position(|message| message.blocks.iter().any(|block| block.id == target))
            {
                let top = self.message_layouts[index].top;
                self.follow_tail = false;
                self.conversation_scroll =
                    (top - area.h * 0.25).clamp(0.0, (self.conversation_extent - area.h).max(0.0));
            }
        }
        let clip = vello::kurbo::Rect::new(
            area.x as f64,
            area.y as f64,
            (area.x + area.w) as f64,
            (area.y + area.h) as f64,
        );
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        self.draw_new_divider(canvas, root, area);
        let p = theme::p();
        let now = Instant::now();
        let mut card_tiles: Vec<(Rect, attachments::Tile)> = Vec::new();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, y, h, message);
            if card.y + card.h >= area.y && card.y <= area.y + area.h {
                if message.role == ChatRole::User {
                    scene::fill(
                        canvas,
                        root,
                        p.row_selected,
                        &scene::rrect(card.x, card.y, card.w, card.h, 8.0),
                    );
                    // Its pictures under its words, drawn once the cards are.
                    if !message.attachments.is_empty() {
                        let top = if message.display.trim().is_empty() {
                            card.y + PAD
                        } else {
                            card.y + PAD + message.layout.height() + attachments::TILE_GAP
                        };
                        let (rects, _) =
                            attachments::card_tiles(&message.attachments, message.wrap_width);
                        for (rect, tile) in rects.iter().zip(&message.attachments) {
                            card_tiles.push((
                                Rect::new(card.x + PAD + rect.x, top + rect.y, rect.w, rect.h),
                                tile.clone(),
                            ));
                        }
                    }
                }
                if message.blocks.is_empty() {
                    if message.state == MessageState::Pending && message.display.is_empty() {
                        if let Some(pulse) = &self.pulse {
                            pulse.draw(
                                canvas,
                                root,
                                Rect::new(
                                    card.x + PAD,
                                    card.y + PAD,
                                    20.0,
                                    role::ui().metrics.line_height,
                                ),
                                now,
                                p.text_dim,
                            );
                        }
                    } else {
                        self.draw_response_selection(
                            canvas,
                            root,
                            message_index,
                            None,
                            card.x + PAD,
                            card.y + PAD,
                        );
                        scene::draw_layout(
                            canvas,
                            root,
                            &message.layout,
                            card.x + PAD,
                            card.y + PAD,
                            Some(p.text),
                        );
                    }
                } else {
                    let mut block_y = card.y + PAD;
                    for block in &message.blocks {
                        let highlighted =
                            self.highlight.is_some_and(|(target, _)| target == block.id);
                        if highlighted {
                            scene::fill(
                                canvas,
                                root,
                                p.row_selected,
                                &scene::rrect(
                                    card.x + 7.0,
                                    block_y - 3.0,
                                    card.w - 14.0,
                                    block.height() + 6.0,
                                    5.0,
                                ),
                            );
                        }
                        let block_index = message
                            .blocks
                            .iter()
                            .position(|candidate| candidate.id == block.id)
                            .unwrap_or(0);
                        if let Some(table) = &block.table {
                            koru_markdown::table::draw_grid(
                                canvas,
                                root,
                                table,
                                card.x + PAD,
                                block_y,
                                0.0,
                                None,
                            );
                            // Over the grid, which fills its header row.
                            self.draw_response_link_hover(
                                canvas,
                                root,
                                block,
                                card.x + PAD,
                                block_y,
                            );
                        } else if let Some(image) = &block.image {
                            self.draw_remote_image(
                                canvas,
                                root,
                                image,
                                Rect::new(
                                    card.x + PAD,
                                    block_y,
                                    card.w - PAD * 2.0,
                                    block.height(),
                                ),
                                &block.layout,
                                Rect::new(
                                    card.x + PAD,
                                    block_y,
                                    card.w - PAD * 2.0,
                                    block.height(),
                                )
                                .contains(self.pointer.0, self.pointer.1),
                            );
                        } else {
                            self.draw_response_link_hover(
                                canvas,
                                root,
                                block,
                                card.x + PAD,
                                block_y,
                            );
                            self.draw_response_selection(
                                canvas,
                                root,
                                message_index,
                                Some(block_index),
                                card.x + PAD,
                                block_y,
                            );
                            scene::draw_layout(
                                canvas,
                                root,
                                &block.layout,
                                card.x + PAD,
                                block_y,
                                Some(p.text),
                            );
                        }
                        block_y += block.height() + BLOCK_GAP;
                    }
                }
                if let Some(usage) = &message.usage {
                    scene::draw_layout(
                        canvas,
                        root,
                        usage,
                        card.x + PAD,
                        card.y + PAD + message_content_height(message) + USAGE_GAP,
                        Some(p.text_dim),
                    );
                }
                if let Some(sources) = &message.sources {
                    let usage_h = message
                        .usage
                        .as_ref()
                        .map_or(0.0, |usage| usage.height() + USAGE_GAP);
                    scene::draw_layout(
                        canvas,
                        root,
                        &sources.layout,
                        card.x + PAD,
                        card.y + PAD + message_content_height(message) + usage_h + USAGE_GAP,
                        Some(p.accent),
                    );
                }
                if message.role == ChatRole::Assistant
                    && message.id.is_some()
                    && self.response_message_hover == Some(message_index)
                {
                    if message.state == MessageState::Complete {
                        for index in 0..RESPONSE_ACTIONS {
                            let rect = action_rect(card, index);
                            IconButton::new(response_action_glyph(index), rect)
                                .with_state(IconButtonState {
                                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                                    active: false,
                                    disabled: index == 3 && message.blocks.is_empty(),
                                })
                                .draw(canvas, root, &mut self.engine);
                        }
                    } else if matches!(
                        message.state,
                        MessageState::Failed | MessageState::Interrupted
                    ) {
                        let rect = action_rect(card, 0);
                        IconButton::new(icons::REDO, rect)
                            .with_state(IconButtonState {
                                hovered: rect.contains(self.pointer.0, self.pointer.1),
                                active: false,
                                disabled: false,
                            })
                            .draw(canvas, root, &mut self.engine);
                    }
                }
            }
        }
        let now_ms = koru_ai_core::Timestamp::now().millis();
        // How far the light has crossed a running reply's header (D83):
        // still under reduced motion.
        let sweep = (!self.reduced_motion).then(|| chat_status::sweep_phase(now_ms));
        let images = &self.images;
        let mut picture = |canvas: &mut Scene, root: Affine, key: &str, rect: Rect| {
            if let Some(loaded) = images.get(key).and_then(|image| image.ready.as_ref()) {
                draw_image_fitted(canvas, root, loaded, rect);
            }
        };
        for notice in &self.notice_layouts {
            let y = area.y - self.conversation_scroll + notice.top;
            if y + notice.layout.height() < area.y || y > area.y + area.h {
                continue;
            }
            scene::draw_layout(
                canvas,
                root,
                &notice.layout,
                area.x + PAD,
                y,
                Some(p.text_dim),
            );
        }
        for work in &self.work_layouts {
            let origin = Rect::new(
                area.x + PAD,
                area.y - self.conversation_scroll + work.top,
                (area.w - PAD * 2.0).max(1.0),
                work.height,
            );
            if origin.y + origin.h < area.y || origin.y > area.y + area.h {
                continue;
            }
            work_view::draw(
                canvas,
                root,
                &mut self.engine,
                self.lang,
                work,
                origin,
                self.work_hover,
                self.focus == Focus::Work(work.message),
                now_ms,
                sweep,
                &mut picture,
            );
        }
        for tool in &self.tool_activity_layouts {
            let row = Rect::new(
                area.x,
                area.y - self.conversation_scroll + tool.top,
                area.w,
                tool.expanded_height,
            );
            if row.y + row.h >= area.y
                && row.y <= area.y + area.h
                && questions::is_question_row(&tool.activity)
            {
                let focused = self.question_focus(tool.activity.id);
                questions::draw_card(
                    canvas,
                    root,
                    &mut self.engine,
                    &self.question_cards,
                    self.lang,
                    self.pointer,
                    focused,
                    row,
                    &tool.activity,
                );
                continue;
            }
            if row.y + row.h >= area.y && row.y <= area.y + area.h {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
                scene::stroke(
                    canvas,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
                let icon = self.engine.icon_layout(icons::WRENCH, 14.0);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &icon,
                    row.x + 9.0,
                    row.y + 9.0,
                    Some(p.text_dim),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &tool.title,
                    row.x + 31.0,
                    row.y + 8.0,
                    Some(p.text),
                );
                scene::draw_layout(
                    canvas,
                    root,
                    &tool.detail,
                    row.x + 12.0,
                    row.y + 35.0,
                    Some(p.text_dim),
                );
                if is_agent_row(&tool.activity) {
                    agents::draw_agent_choices(
                        canvas,
                        root,
                        &mut self.engine,
                        self.lang,
                        self.pointer,
                        row,
                        &tool.activity,
                    );
                } else {
                    let [approve, deny] = tool_activity_action_rects(row);
                    draw_surface_button(
                        canvas,
                        root,
                        &mut self.engine,
                        approve,
                        koru_i18n::t(self.lang, koru_i18n::Key::AiToolApprove),
                        ButtonRole::Primary,
                        approve.contains(self.pointer.0, self.pointer.1),
                        false,
                    );
                    draw_surface_button(
                        canvas,
                        root,
                        &mut self.engine,
                        deny,
                        koru_i18n::t(self.lang, koru_i18n::Key::AiToolDeny),
                        ButtonRole::Secondary,
                        deny.contains(self.pointer.0, self.pointer.1),
                        false,
                    );
                }
            }
        }
        card_tiles.retain(|(rect, _)| rect.y + rect.h >= area.y && rect.y <= area.y + area.h);
        self.draw_card_tiles(canvas, root, &card_tiles);
        self.card_tile_hits.clone_from(&card_tiles);
        canvas.pop_layer();
        self.request_thumbnails(&card_tiles);
        if let Some(scrollbar) = self.conversation_scrollbar() {
            let active = self.conversation_scroll_drag.is_some()
                || scrollbar.in_gutter(self.pointer.0, self.pointer.1);
            koru_ui::scrollbar::draw(canvas, root, scrollbar, active);
        }
        self.draw_jump_button(canvas, root);
        self.draw_destination_picker(canvas, root);
        self.copy_feedback.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
    }

    fn response_action_at(&mut self, x: f32, y: f32) -> Option<ResponseAction> {
        let area = self.conversation_rect();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if let Some(message_id) = message.id
                && message.role == ChatRole::Assistant
                && self.response_message_hover == Some(message_index)
            {
                if message.state == MessageState::Complete {
                    for index in 0..RESPONSE_ACTIONS {
                        if action_rect(card, index).contains(x, y) {
                            return match index {
                                0 => Some(ResponseAction::Copy(message.markdown.clone())),
                                1 => Some(ResponseAction::Branch(message_id)),
                                2 => Some(ResponseAction::Store(message_id)),
                                3 => message
                                    .blocks
                                    .first()
                                    .map(|block| ResponseAction::Link(block.id)),
                                _ => None,
                            };
                        }
                    }
                } else if matches!(
                    message.state,
                    MessageState::Failed | MessageState::Interrupted
                ) && action_rect(card, 0).contains(x, y)
                {
                    return Some(ResponseAction::Retry(message_id));
                }
            }
        }
        None
    }

    fn source_link_at(&self, x: f32, y: f32) -> Option<String> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for message in &self.message_layouts {
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, message_height(message), message);
            let Some(sources) = &message.sources else {
                continue;
            };
            let usage_h = message
                .usage
                .as_ref()
                .map_or(0.0, |usage| usage.height() + USAGE_GAP);
            let origin = (
                card.x + PAD,
                card.y + PAD + message_content_height(message) + usage_h + USAGE_GAP,
            );
            let rect = Rect::new(
                origin.0,
                origin.1,
                card.w - PAD * 2.0,
                sources.layout.height(),
            );
            if rect.contains(x, y) {
                return sources.hit(x - origin.0, y - origin.1).map(str::to_owned);
            }
        }
        None
    }

    /// What in a reply's work view is under the pointer.
    fn work_hit_at(&self, x: f32, y: f32) -> Option<work_view::WorkHit> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        self.work_layouts.iter().find_map(|work| {
            work_view::hit(
                work,
                area.x + PAD,
                area.y - self.conversation_scroll + work.top,
                (area.w - PAD * 2.0).max(1.0),
                x,
                y,
            )
        })
    }

    /// Open or fold what was hit: a reply's work, a step group, a call.
    fn toggle_work(&mut self, hit: work_view::WorkHit) -> bool {
        fn flip<T: std::hash::Hash + Eq>(set: &mut HashSet<T>, value: T) {
            if !set.remove(&value) {
                set.insert(value);
            }
        }
        match hit {
            work_view::WorkHit::Header(message) => flip(&mut self.opened_work, message),
            work_view::WorkHit::Group(message, index) => {
                flip(&mut self.opened_step_groups, (message, index));
            }
            work_view::WorkHit::Call(call) => flip(&mut self.opened_calls, call),
            work_view::WorkHit::Toggle(call) => flip(&mut self.expanded_outputs, call),
            work_view::WorkHit::Link(..) => {
                if let Some(url) = self
                    .work_layouts
                    .iter()
                    .flat_map(|work| &work.lines)
                    .find(|line| line.hit == Some(hit))
                    .and_then(|line| line.url.as_deref())
                {
                    open_web_link(url);
                }
                return true;
            }
            // A changed file's path (D62): the file, or its folder.
            work_view::WorkHit::Path(..) => {
                if let Some(path) = self
                    .work_layouts
                    .iter()
                    .flat_map(|work| &work.lines)
                    .find(|line| line.hit == Some(hit))
                    .and_then(|line| line.url.clone())
                {
                    self.open_changed_file(&path);
                }
                return true;
            }
        }
        self.messages_dirty = true;
        true
    }

    /// Enter or Space opens the focused reply's work; Escape gives the
    /// keyboard back to the composer. Anything else is the composer's.
    fn work_key(&mut self, message: MessageId, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !mods.is_empty() {
            return None;
        }
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                Some(self.toggle_work(work_view::WorkHit::Header(message)))
            }
            Key::Character(value) if value.as_str() == " " => {
                Some(self.toggle_work(work_view::WorkHit::Header(message)))
            }
            Key::Named(NamedKey::Escape) => {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                Some(true)
            }
            _ => None,
        }
    }

    /// When a running reply's *Working for …* moves on next: once a second
    /// for its count, or at the status frame while its light sweeps
    /// (koru-ai D83) — never while no reply runs.
    fn work_tick_deadline(&self) -> Option<Instant> {
        let step = if self.reduced_motion {
            Duration::from_secs(1)
        } else {
            koru_chats_ui::status::FRAME
        };
        self.work_layouts.iter().any(|work| work.live).then(|| {
            self.work_ticked_at
                .map_or_else(Instant::now, |at| at + step)
        })
    }

    /// Part 0 is the card, 1 is Approve, 2 is Deny, and an agent's choices
    /// count from `agents::CHOICE_PART_BASE`.
    fn tool_activity_at(&self, x: f32, y: f32) -> Option<(ToolInvocationId, u8)> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for tool in &self.tool_activity_layouts {
            let row = Rect::new(
                area.x,
                area.y - self.conversation_scroll + tool.top,
                area.w,
                tool.expanded_height,
            );
            if !row.contains(x, y) {
                continue;
            }
            // A question card answers its own presses (D108); a closed
            // set's summary has nothing to press.
            if questions::is_question_row(&tool.activity) {
                return Some((tool.activity.id, 0));
            }
            if is_agent_row(&tool.activity) {
                if let Some(part) = agents::agent_choice_at(row, &tool.activity, x, y) {
                    return Some((tool.activity.id, part));
                }
                return Some((tool.activity.id, 0));
            }
            let [approve, deny] = tool_activity_action_rects(row);
            if approve.contains(x, y) {
                return Some((tool.activity.id, 1));
            }
            if deny.contains(x, y) {
                return Some((tool.activity.id, 2));
            }
            return Some((tool.activity.id, 0));
        }
        None
    }

    fn response_action_hover_at(&mut self, x: f32, y: f32) -> Option<(usize, u8)> {
        let area = self.conversation_rect();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant
                && message.id.is_some()
                && self.response_message_hover == Some(message_index)
            {
                let action_count = if message.state == MessageState::Complete {
                    RESPONSE_ACTIONS
                } else if matches!(
                    message.state,
                    MessageState::Failed | MessageState::Interrupted
                ) {
                    1
                } else {
                    0
                };
                for index in 0..action_count {
                    if action_rect(card, index).contains(x, y) {
                        return Some((message_index, 64 + index as u8));
                    }
                }
            }
        }
        None
    }

    fn response_message_hover_at(
        &mut self,
        x: f32,
        y: f32,
        previous: Option<usize>,
    ) -> Option<usize> {
        let area = self.conversation_rect();
        for (index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant && message.id.is_some() {
                let actions = message
                    .state
                    .is_terminal()
                    .then_some(ACTION_H + 8.0)
                    .unwrap_or(0.0);
                let text = Rect::new(card.x, card.y, card.w, (card.h - actions).max(0.0));
                if text.contains(x, y) || (previous == Some(index) && card.contains(x, y)) {
                    return Some(index);
                }
            }
        }
        None
    }

    fn response_text_hit(&self, x: f32, y: f32) -> Option<ResponseTextHit> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            let h = message_height(message);
            let card_y = area.y - self.conversation_scroll + message.top;
            let card = message_card(area, card_y, h, message);
            if message.role == ChatRole::Assistant {
                if message.blocks.is_empty() {
                    let origin = (card.x + PAD, card.y + PAD);
                    let rect = Rect::new(
                        origin.0,
                        origin.1,
                        card.w - PAD * 2.0,
                        message.layout.height(),
                    );
                    if rect.contains(x, y) {
                        let selection =
                            Selection::from_point(&message.layout, x - origin.0, y - origin.1);
                        return Some(ResponseTextHit {
                            position: ResponsePosition {
                                message: message_index,
                                block: None,
                                byte: selection.focus().index(),
                                affinity: selection.focus().affinity(),
                            },
                            selection,
                            link: None,
                        });
                    }
                } else {
                    let mut block_y = card.y + PAD;
                    for (block_index, block) in message.blocks.iter().enumerate() {
                        let origin = (card.x + PAD, block_y);
                        let rect =
                            Rect::new(origin.0, origin.1, card.w - PAD * 2.0, block.height());
                        if block.image.is_some() && rect.contains(x, y) {
                            let selection =
                                Selection::from_point(&block.layout, x - origin.0, y - origin.1);
                            return Some(ResponseTextHit {
                                position: ResponsePosition {
                                    message: message_index,
                                    block: Some(block_index),
                                    byte: selection.focus().index(),
                                    affinity: selection.focus().affinity(),
                                },
                                selection,
                                link: block.image.as_ref().map(|image| image.url.clone()),
                            });
                        }
                        // A table is not selected through, but a link in
                        // one of its cells is pressed and followed as one
                        // in a sentence is (D95).
                        if block.table.is_some() && rect.contains(x, y) {
                            if let Some(link) = block.link_at(x - origin.0, y - origin.1) {
                                return Some(ResponseTextHit {
                                    position: ResponsePosition {
                                        message: message_index,
                                        block: Some(block_index),
                                        byte: link.range.start,
                                        affinity: Affinity::Downstream,
                                    },
                                    selection: Selection::from_point(&block.layout, 0.0, 0.0),
                                    link: Some(link.url.to_owned()),
                                });
                            }
                        }
                        if block.table.is_none() && rect.contains(x, y) {
                            let selection =
                                Selection::from_point(&block.layout, x - origin.0, y - origin.1);
                            let link = block
                                .link_at(x - origin.0, y - origin.1)
                                .map(|link| link.url.to_owned());
                            return Some(ResponseTextHit {
                                position: ResponsePosition {
                                    message: message_index,
                                    block: Some(block_index),
                                    byte: selection.focus().index(),
                                    affinity: selection.focus().affinity(),
                                },
                                selection,
                                link,
                            });
                        }
                        block_y += block.height() + BLOCK_GAP;
                    }
                }
            }
        }
        None
    }

    fn response_layout(
        &self,
        message_index: usize,
        block_index: Option<usize>,
    ) -> Option<(&Layout<Brush>, (f32, f32))> {
        let area = self.conversation_hit_rect;
        let message = self.message_layouts.get(message_index)?;
        let card = message_card(
            area,
            area.y - self.conversation_scroll + message.top,
            message_height(message),
            message,
        );
        match block_index {
            None => Some((&message.layout, (card.x + PAD, card.y + PAD))),
            Some(index) => {
                let block_y = card.y
                    + PAD
                    + message
                        .blocks
                        .get(..index)?
                        .iter()
                        .map(|block| block.height() + BLOCK_GAP)
                        .sum::<f32>();
                Some((&message.blocks.get(index)?.layout, (card.x + PAD, block_y)))
            }
        }
    }

    fn drag_response_selection(&mut self, x: f32, y: f32) -> bool {
        let Some(previous) = self
            .response_selection
            .as_ref()
            .filter(|selection| selection.dragging)
            .map(|selection| selection.focus)
        else {
            return false;
        };
        let Some(hit) = self.response_text_hit(x, y) else {
            return false;
        };
        let next = hit.position;
        let changed = next != previous;
        if let Some(target) = &mut self.response_selection {
            target.focus = next;
            if changed {
                target.pressed_link = None;
            }
        }
        changed
    }

    fn selected_response_text(&self) -> Option<String> {
        let selected = self.response_selection.as_ref()?;
        if selected.anchor == selected.focus {
            return None;
        }
        let mut pieces = Vec::new();
        for (message, block) in self.response_fragments() {
            let Some(text) = self.response_text(message, block) else {
                continue;
            };
            let Some(range) = self.response_selection_range(message, block, text.len()) else {
                continue;
            };
            if let Some(piece) = text.get(range)
                && !piece.is_empty()
            {
                pieces.push(piece);
            }
        }
        (!pieces.is_empty()).then(|| pieces.join("\n\n"))
    }

    fn draw_response_selection(
        &self,
        canvas: &mut Scene,
        root: Affine,
        message: usize,
        block: Option<usize>,
        ox: f32,
        oy: f32,
    ) {
        let Some(layout) = self
            .message_layouts
            .get(message)
            .and_then(|message| match block {
                Some(index) => message.blocks.get(index).map(|block| &block.layout),
                None => Some(&message.layout),
            })
        else {
            return;
        };
        let Some(text) = self.response_text(message, block) else {
            return;
        };
        let Some(range) = self.response_selection_range(message, block, text.len()) else {
            return;
        };
        let selection = Selection::new(
            Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
            Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
        );
        selection.geometry_with(layout, |bb, _| {
            scene::fill(
                canvas,
                root,
                theme::selection_color(),
                &scene::rect(
                    ox + bb.x0 as f32,
                    oy + bb.y0 as f32,
                    (bb.x1 - bb.x0) as f32,
                    (bb.y1 - bb.y0) as f32,
                ),
            );
        });
    }

    fn response_text(&self, message: usize, block: Option<usize>) -> Option<&str> {
        let message = self.message_layouts.get(message)?;
        match block {
            None if message.blocks.is_empty() => Some(&message.markdown),
            Some(index) => message.blocks.get(index).and_then(|block| {
                (block.table.is_none() && block.image.is_none()).then_some(block.text.as_str())
            }),
            None => None,
        }
    }

    fn response_fragments(&self) -> Vec<(usize, Option<usize>)> {
        let mut fragments = Vec::new();
        for (message_index, message) in self.message_layouts.iter().enumerate() {
            if message.role != ChatRole::Assistant {
                continue;
            }
            if message.blocks.is_empty() {
                fragments.push((message_index, None));
            } else {
                fragments.extend(
                    message
                        .blocks
                        .iter()
                        .enumerate()
                        .filter(|(_, block)| block.table.is_none() && block.image.is_none())
                        .map(|(index, _)| (message_index, Some(index))),
                );
            }
        }
        fragments
    }

    fn response_position_key(position: ResponsePosition) -> (usize, usize, usize) {
        (position.message, position.block.unwrap_or(0), position.byte)
    }

    fn response_selection_range(
        &self,
        message: usize,
        block: Option<usize>,
        text_len: usize,
    ) -> Option<std::ops::Range<usize>> {
        let selected = self.response_selection.as_ref()?;
        let (start, end) = if Self::response_position_key(selected.anchor)
            <= Self::response_position_key(selected.focus)
        {
            (selected.anchor, selected.focus)
        } else {
            (selected.focus, selected.anchor)
        };
        let fragment = (message, block.unwrap_or(0));
        let first = (start.message, start.block.unwrap_or(0));
        let last = (end.message, end.block.unwrap_or(0));
        if fragment < first || fragment > last {
            return None;
        }
        let from = if fragment == first {
            start.byte.min(text_len)
        } else {
            0
        };
        let to = if fragment == last {
            end.byte.min(text_len)
        } else {
            text_len
        };
        (from < to).then_some(from..to)
    }

    fn response_selection_contains(&self, x: f32, y: f32) -> bool {
        self.response_fragments()
            .into_iter()
            .any(|(message, block)| {
                let Some((layout, origin)) = self.response_layout(message, block) else {
                    return false;
                };
                let Some(text) = self.response_text(message, block) else {
                    return false;
                };
                let Some(range) = self.response_selection_range(message, block, text.len()) else {
                    return false;
                };
                let selection = Selection::new(
                    Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
                    Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
                );
                let mut contains = false;
                selection.geometry_with(layout, |bb, _| {
                    contains |= Rect::new(
                        origin.0 + bb.x0 as f32,
                        origin.1 + bb.y0 as f32,
                        (bb.x1 - bb.x0) as f32,
                        (bb.y1 - bb.y0) as f32,
                    )
                    .contains(x, y);
                });
                contains
            })
    }

    fn move_response_selection(&mut self, key: &Key) -> bool {
        let Some(focus) = self
            .response_selection
            .as_ref()
            .map(|selection| selection.focus)
        else {
            return false;
        };
        let Some((layout, _)) = self.response_layout(focus.message, focus.block) else {
            return false;
        };
        let current = Selection::from_byte_index(layout, focus.byte, focus.affinity);
        let desired_x = current.focus().geometry(layout, 0.0).x0 as f32;
        let vertical = matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown));
        let backwards = matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp));
        let moved = match key {
            Key::Named(NamedKey::ArrowLeft) => current.previous_visual(layout, false),
            Key::Named(NamedKey::ArrowRight) => current.next_visual(layout, false),
            Key::Named(NamedKey::ArrowUp) => current.previous_line(layout, false),
            Key::Named(NamedKey::ArrowDown) => current.next_line(layout, false),
            _ => return false,
        };
        let mut next = ResponsePosition {
            byte: moved.focus().index(),
            affinity: moved.focus().affinity(),
            ..focus
        };
        if next.byte == focus.byte {
            let fragments = self.response_fragments();
            let Some(index) = fragments
                .iter()
                .position(|&(message, block)| message == focus.message && block == focus.block)
            else {
                return false;
            };
            let neighbour = if backwards {
                index.checked_sub(1)
            } else {
                index.checked_add(1).filter(|&next| next < fragments.len())
            };
            let Some(neighbour) = neighbour else {
                return true;
            };
            let (message, block) = fragments[neighbour];
            let Some(text) = self.response_text(message, block) else {
                return false;
            };
            let byte = if backwards { text.len() } else { 0 };
            next = ResponsePosition {
                message,
                block,
                byte,
                affinity: if backwards {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                },
            };
            if vertical {
                // Preserve the visual column when the line step crosses a
                // Markdown block, just as Parley does within one layout.
                if let Some((neighbour_layout, _)) = self.response_layout(message, block) {
                    let y = if backwards {
                        (neighbour_layout.height() - 0.5).max(0.0)
                    } else {
                        0.0
                    };
                    let cursor = Selection::from_point(neighbour_layout, desired_x, y).focus();
                    next.byte = cursor.index();
                    next.affinity = cursor.affinity();
                }
            }
        }
        if let Some(selection) = &mut self.response_selection {
            selection.focus = next;
            selection.pressed_link = None;
        }
        true
    }

    fn menu_viewport(&self) -> (f32, f32) {
        (
            self.viewport.x + self.viewport.w,
            self.viewport.y + self.viewport.h,
        )
    }

    fn draw_response_link_hover(
        &self,
        canvas: &mut Scene,
        root: Affine,
        block: &BlockLayout,
        ox: f32,
        oy: f32,
    ) {
        // Every block is asked, so the pointer must be on this one.
        let Some(link) = block.link_at(self.pointer.0 - ox, self.pointer.1 - oy) else {
            return;
        };
        // A link in a table's cell is drawn in that cell's layout.
        let (ox, oy) = (ox + link.origin.0, oy + link.origin.1);
        let selection = Selection::new(
            Cursor::from_byte_index(link.layout, link.range.start, Affinity::Downstream),
            Cursor::from_byte_index(link.layout, link.range.end, Affinity::Upstream),
        );
        selection.geometry_with(link.layout, |bb, _| {
            scene::fill(
                canvas,
                root,
                theme::p().hover_tint,
                &scene::rrect(
                    ox + bb.x0 as f32 - 1.0,
                    oy + bb.y0 as f32,
                    (bb.x1 - bb.x0) as f32 + 2.0,
                    (bb.y1 - bb.y0) as f32,
                    2.0,
                ),
            );
        });
    }

    fn ensure_remote_image(&mut self, url: &str) {
        if self.images.contains_key(url) || !is_web_url(url) {
            return;
        }
        let worker_url = url.to_owned();
        self.start_image_load(url.to_owned(), move || load_remote_image(&worker_url));
    }

    /// **A picture a call read, from the disk** (D38): read and decoded
    /// the way a reply's picture is, off this thread, once. Under test
    /// nothing is read: the picture counts as not there.
    fn ensure_disk_image(&mut self, path: PathBuf) {
        let key = path.to_string_lossy().into_owned();
        if self.images.contains_key(&key) {
            return;
        }
        if cfg!(test) {
            self.images.insert(
                key,
                ImageLoad {
                    pending: None,
                    ready: None,
                    failed: true,
                },
            );
            return;
        }
        self.start_image_load(key, move || load_disk_image(&path));
    }

    /// Run `load` on a `koru-ai-image` thread, keeping its picture under
    /// `key` once [`Self::maintain_images`] collects it.
    fn start_image_load(
        &mut self,
        key: String,
        load: impl FnOnce() -> ImageResult + Send + 'static,
    ) {
        let result = Arc::new(Mutex::new(None));
        let worker_result = result.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-ai-image".into())
            .spawn(move || {
                let loaded = load();
                if let Ok(mut slot) = worker_result.lock() {
                    *slot = Some(loaded);
                }
            });
        self.images.insert(
            key,
            ImageLoad {
                failed: spawned.is_err(),
                pending: spawned.is_ok().then_some(result),
                ready: None,
            },
        );
    }

    fn maintain_images(&mut self) -> bool {
        let mut changed = false;
        let mut relayout = false;
        for (key, image) in &mut self.images {
            let completed = image
                .pending
                .as_ref()
                .and_then(|pending| pending.lock().ok()?.take());
            if let Some(completed) = completed {
                image.pending = None;
                match completed {
                    Ok(loaded) => image.ready = Some(loaded),
                    Err(_) => image.failed = true,
                }
                changed = true;
                // A reply's picture has its box already; a call's takes
                // room only once it has loaded (D38). A picture from the
                // disk may be either, so the messages are laid out again.
                relayout |= !is_web_url(key);
            }
        }
        if relayout {
            self.messages_dirty = true;
        }
        changed
    }

    fn draw_remote_image(
        &self,
        canvas: &mut Scene,
        root: Affine,
        image: &ImageSpec,
        rect: Rect,
        fallback: &Layout<Brush>,
        hovered: bool,
    ) {
        let Some(loaded) = self
            .images
            .get(&image_key(&image.url))
            .and_then(|image| image.ready.as_ref())
        else {
            scene::fill(
                canvas,
                root,
                theme::p().code_bg,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
            );
            scene::draw_layout(
                canvas,
                root,
                fallback,
                rect.x + PAD,
                rect.y + PAD,
                Some(theme::p().text_dim),
            );
            if hovered {
                scene::stroke(
                    canvas,
                    root,
                    theme::p().focus_ring,
                    1.0,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            return;
        };
        draw_image_fitted(canvas, root, loaded, rect);
        if hovered {
            scene::stroke(
                canvas,
                root,
                theme::p().focus_ring,
                1.0,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
            );
        }
    }

    fn perform_response_action(&mut self, action: ResponseAction) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.clone())
        else {
            return false;
        };
        match action {
            ResponseAction::Copy(markdown) => {
                koru_ui::clipboard::put_plain(&markdown);
                self.show_copy_feedback("Copied");
                true
            }
            ResponseAction::Branch(message) => self.send_command(Command::Branch(
                chat.id,
                BranchChatRequest {
                    branch_chat_id: ChatId::new(),
                    through_message_id: message,
                    title: format!("{} — branch", chat.title),
                },
            )),
            ResponseAction::Store(message) => {
                if self.destinations.is_empty() {
                    self.status = Some("No writable Koru workspace is available.".into());
                } else {
                    self.destination_open = Some(message);
                }
                true
            }
            ResponseAction::Link(block) => {
                koru_ui::clipboard::put_plain(&koru_link::chat_link(
                    chat.id.as_uuid(),
                    Some(block.as_uuid()),
                ));
                self.show_copy_feedback(koru_i18n::t(self.lang, koru_i18n::Key::AiLinkCopied));
                true
            }
            ResponseAction::Retry(message) => {
                let (explicit_refs, _) = self.turn_refs(&[]);
                self.send_command(Command::Retry(
                    chat.id,
                    RetryTurnRequest {
                        operation_id: uuid::Uuid::new_v4(),
                        message_id: message,
                        surface: SURFACE,
                        explicit_refs,
                        web_search: self.web_search_armed && self.web_search_available(),
                        reasoning_override: None,
                        replace_pinned_documents: true,
                    },
                ))
            }
        }
    }

    fn show_copy_feedback(&mut self, label: &str) {
        let (x, y) = self.pointer;
        self.copy_feedback.show_transient(
            TooltipContent::new(1, Rect::new(x, y, 1.0, 1.0), label),
            Instant::now(),
            Duration::from_millis(900),
        );
    }

    fn start_export(&mut self, through_message_id: MessageId) -> bool {
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.clone())
        else {
            return false;
        };
        let Some(destination) = self.destinations.get(self.selected_destination).cloned() else {
            self.status = Some("The selected Koru workspace is unavailable.".into());
            return true;
        };
        self.destination_open = None;
        self.send_command(Command::Export(
            chat.id,
            ExportChatRequest {
                export_id: DocumentExportId::new(),
                idempotency_key: uuid::Uuid::new_v4(),
                through_message_id,
                destination: DocumentDestination {
                    profile_id: destination.profile_id,
                    workspace_id: destination.workspace_id,
                    parent_document_id: None,
                    index: None,
                    title: chat.title,
                },
            },
        ))
    }

    fn draw_destination_picker(&mut self, canvas: &mut Scene, root: Affine) {
        if self.destination_open.is_none() {
            return;
        }
        let panel = destination_panel(self.viewport, self.destinations.len());
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let p = theme::p();
        for (index, destination) in self.destinations.iter().take(8).enumerate() {
            let row = destination_row(panel, index);
            if row.contains(self.pointer.0, self.pointer.1) || index == self.selected_destination {
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(row.x, row.y, row.w, row.h, 6.0),
                );
            }
            let label =
                self.engine
                    .simple_layout_wrapped(&destination.label, role::row(), row.w - 20.0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                row.x + 10.0,
                row.y + 9.0,
                Some(p.text),
            );
        }
    }

    fn destination_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = destination_panel(self.viewport, self.destinations.len());
        (0..self.destinations.len().min(8))
            .find(|index| destination_row(panel, *index).contains(x, y))
    }

    fn draw_setup(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let title = self.engine.simple_layout(
            if self.transcript.is_some() {
                "Settings"
            } else {
                "Set up Koru AI"
            },
            role::section_heading(),
        );
        let content = self.setup_content_rect();
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            content.x,
            self.viewport.y + 16.0,
            Some(p.text),
        );
        if self.setup_open && self.transcript.is_some() {
            let close = self.setup_close_button();
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                close,
                "Done",
                ButtonRole::Secondary,
                close.contains(self.pointer.0, self.pointer.1),
                false,
            );
        }
        let cards = self.provider_cards();
        let koru_label = koru_i18n::t(self.lang, koru_i18n::Key::AiKoruProvider);
        let agents_label = koru_i18n::t(self.lang, koru_i18n::Key::AiLocalAgents);
        let has_agents = self
            .settings
            .as_ref()
            .is_some_and(|settings| !settings.agents.is_empty());
        for (index, (label, enabled, selected)) in [
            (koru_label, true, self.setup_provider == SetupProvider::Koru),
            ("OpenAI", true, self.setup_provider == SetupProvider::OpenAi),
            (
                "On this device — Ollama",
                true,
                self.setup_provider == SetupProvider::Ollama,
            ),
            (
                agents_label,
                has_agents,
                self.setup_provider == SetupProvider::Agents,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = cards[index];
            scene::fill(
                canvas,
                root,
                if selected {
                    p.row_selected
                } else if enabled && rect.contains(self.pointer.0, self.pointer.1) {
                    p.hover_tint
                } else {
                    p.panel_bg
                },
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if selected {
                    p.focus_ring
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
            );
            let line = self.engine.simple_layout(label, role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                rect.x + 12.0,
                rect.y + 13.0,
                Some(if enabled { p.text } else { p.text_dim }),
            );
        }
        let fields = self.field_rects();
        let style = InputStyle::inline();
        match self.setup_provider {
            SetupProvider::OpenAi => {
                let configured = self.openai_credential_configured();
                let mut key_field = fields[0];
                if configured && !self.editing_openai_key {
                    key_field.w = (key_field.w - 112.0).max(80.0);
                }
                self.openai_key.draw_masked(
                    canvas,
                    root,
                    &mut self.engine,
                    key_field,
                    style,
                    Some(if configured && !self.editing_openai_key {
                        "API key saved securely"
                    } else {
                        "Paste an OpenAI project API key"
                    }),
                    self.focus == Focus::OpenAiKey,
                );
                if configured && !self.editing_openai_key {
                    for (rect, label) in [
                        (self.openai_credential_replace_button(), "Replace"),
                        (self.openai_credential_clear_button(), "Clear"),
                    ] {
                        draw_surface_button(
                            canvas,
                            root,
                            &mut self.engine,
                            rect,
                            label,
                            if label == "Clear" {
                                ButtonRole::Destructive
                            } else {
                                ButtonRole::Secondary
                            },
                            rect.contains(self.pointer.0, self.pointer.1),
                            false,
                        );
                    }
                }
                self.openai_model.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    fields[1],
                    style,
                    Some("Model, for example gpt-5.1"),
                    self.focus == Focus::OpenAiModel,
                );
            }
            SetupProvider::Ollama => {
                self.ollama_endpoint.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    fields[0],
                    style,
                    Some("Ollama endpoint"),
                    self.focus == Focus::OllamaEndpoint,
                );
                let mut model_field = fields[1];
                if self.ollama_pull_action_rect().is_some() {
                    model_field.w = (model_field.w - 68.0).max(80.0);
                }
                self.ollama_model.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    model_field,
                    style,
                    Some("Installed model or model to pull"),
                    self.focus == Focus::OllamaModel,
                );
                if let Some(action) = self.ollama_pull_action_rect() {
                    if action.contains(self.pointer.0, self.pointer.1) {
                        scene::fill(
                            canvas,
                            root,
                            p.hover_tint,
                            &scene::rrect(action.x, action.y, action.w, action.h, 5.0),
                        );
                    }
                    let action_label = if self.pull_active { "Cancel" } else { "Pull" };
                    let label = self.engine.simple_layout(action_label, role::meta());
                    let enter = self.engine.icon_layout(icons::ENTER, 15.0);
                    let content_w = label.width() + enter.width() + 4.0;
                    let x = action.x + (action.w - content_w) * 0.5;
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &label,
                        x,
                        action.y + (action.h - label.height()) * 0.5,
                        Some(p.accent),
                    );
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &enter,
                        x + label.width() + 4.0,
                        action.y + (action.h - enter.height()) * 0.5,
                        Some(p.accent),
                    );
                }
            }
            SetupProvider::Agents => {
                self.draw_agent_rows(canvas, root, fields[0]);
            }
            SetupProvider::Koru => {
                let status = self.engine.simple_layout_wrapped(
                    &koru_i18n::t(self.lang, koru_i18n::Key::AiKoruHostedStatus),
                    role::meta(),
                    fields[0].w,
                );
                scene::draw_layout(
                    canvas,
                    root,
                    &status,
                    fields[0].x,
                    fields[0].y + 8.0,
                    Some(p.text_dim),
                );
            }
        }
        if !self.reasoning_choices().is_empty() {
            let reasoning = self.reasoning_button();
            let label = format!("Reasoning: {}", self.reasoning.as_token());
            draw_surface_button(
                canvas,
                root,
                &mut self.engine,
                reasoning,
                &label,
                ButtonRole::Secondary,
                reasoning.contains(self.pointer.0, self.pointer.1),
                false,
            );
        }
        let save = self.setup_save_button();
        let save_label = if self.transcript.is_some() {
            koru_i18n::t(self.lang, koru_i18n::Key::AiSaveAndCheck)
        } else {
            koru_i18n::t(self.lang, koru_i18n::Key::AiSaveAndStartChat)
        };
        draw_surface_button(
            canvas,
            root,
            &mut self.engine,
            save,
            &save_label,
            ButtonRole::Primary,
            save.contains(self.pointer.0, self.pointer.1),
            false,
        );
        self.ollama_search_link = None;
        if self.setup_provider == SetupProvider::Ollama && self.ollama_models.is_empty() {
            let link = self
                .engine
                .simple_layout("Browse Ollama models", role::meta());
            let x = content.x;
            let y = fields[1].y + fields[1].h + 8.0;
            let hit = Rect::new(x - 3.0, y - 3.0, link.width() + 6.0, link.height() + 6.0);
            if hit.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                );
            }
            scene::draw_layout_first_line(canvas, root, &link, x, y, Some(p.accent));
            self.ollama_search_link = Some(hit);
        }
        if self.transcript.is_some() {
            for (rect, label) in [
                (self.clear_history_button(), "Clear history"),
                (self.clear_usage_button(), "Clear usage"),
            ] {
                draw_surface_button(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    label,
                    ButtonRole::Destructive,
                    rect.contains(self.pointer.0, self.pointer.1),
                    false,
                );
            }
        }
        self.retry_link = None;
        let status = self.pull_status.as_ref().or(self.status.as_ref()).cloned();
        if let Some(status) = status {
            let retryable = self.validation_retryable();
            let layout = self.engine.simple_layout_wrapped(
                &status,
                role::meta(),
                (content.w - if retryable { 52.0 } else { 0.0 }).max(1.0),
            );
            // Under the cards, and under the *Local agents* list when that is
            // what is open: the list grows with the agents installed, and the
            // line was drawn across its fourth row (the review's note).
            scene::draw_layout(
                canvas,
                root,
                &layout,
                content.x,
                self.viewport.y + 454.0 + self.ollama_catalog_gap() + self.agents_gap(),
                Some(p.text_dim),
            );
            if retryable {
                let retry = self.engine.simple_layout("Retry", role::meta());
                let x =
                    (content.x + layout.width() + 8.0).min(content.x + content.w - retry.width());
                let y = self.viewport.y + 454.0 + self.ollama_catalog_gap() + self.agents_gap();
                let hit = Rect::new(x - 3.0, y - 3.0, retry.width() + 6.0, retry.height() + 6.0);
                if hit.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                    );
                }
                scene::draw_layout_first_line(canvas, root, &retry, x, y, Some(p.accent));
                self.retry_link = Some(hit);
            }
        }
        if let Some(prompt) = self.system_prompt_rect() {
            let label = self.engine.simple_layout(
                "System prompt override · this window · debug only",
                role::meta(),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                prompt.x,
                prompt.y - label.height() - 5.0,
                Some(p.text_dim),
            );
            scene::fill(
                canvas,
                root,
                p.panel_bg,
                &scene::rrect(prompt.x, prompt.y, prompt.w, prompt.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if self.focus == Focus::SystemPrompt {
                    p.focus_ring
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(prompt.x, prompt.y, prompt.w, prompt.h, 8.0),
            );
            self.system_prompt.draw(
                canvas,
                root,
                &mut self.engine,
                prompt,
                Some("Leave empty to use Koru's bundled system prompt"),
                self.focus == Focus::SystemPrompt,
            );
        }
        if let Some(panel) = self.model_options_panel() {
            scene::floating_panel(
                canvas,
                root,
                panel.x,
                panel.y,
                panel.w,
                panel.h,
                &scene::Panel::popover(6.0),
            );
            for (index, model) in self
                .model_options()
                .into_iter()
                .enumerate()
                .skip(self.model_option_offset)
                .take(MODEL_VISIBLE_ROWS)
            {
                let visible = index.saturating_sub(self.model_option_offset);
                let row = Rect::new(
                    panel.x + 4.0,
                    panel.y + 4.0 + visible as f32 * 30.0,
                    panel.w - 8.0,
                    30.0,
                );
                if row.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(row.x, row.y, row.w, row.h, 4.0),
                    );
                }
                let line = self.engine.simple_layout(&model, role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &line,
                    row.x + 8.0,
                    row.y + (row.h - line.height()) * 0.5,
                    Some(p.text),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_surface_button(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    label: &str,
    role: ButtonRole,
    hovered: bool,
    selected: bool,
) {
    if selected {
        scene::fill(
            canvas,
            root,
            theme::p().row_selected,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, ActionButton::RADIUS),
        );
    }
    let style = match role {
        ButtonRole::Primary => ActionStyle::Primary,
        ButtonRole::Secondary => ActionStyle::Ghost,
        ButtonRole::Destructive => ActionStyle::Destructive,
    };
    ActionButton::new(label, style, None).draw(canvas, root, engine, rect, hovered);
}

fn draw_composer_action(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    mode: queue::SendMode,
    enabled: bool,
    hovered: bool,
) {
    let p = theme::p();
    let stop = mode == queue::SendMode::Stop;
    let fill = if stop {
        if hovered { p.hover_tint } else { p.panel_bg }
    } else if enabled {
        p.accent
    } else {
        p.row_selected
    };
    scene::fill(
        canvas,
        root,
        fill,
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5),
    );
    if stop {
        scene::stroke(
            canvas,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5),
        );
    }
    let glyph = engine.icon_layout(
        match mode {
            queue::SendMode::Stop => icons::SQUARE,
            // Queue: kept for after the reply.
            queue::SendMode::Queue => icons::CLOCK,
            queue::SendMode::Send => icons::ARROW_UP,
        },
        15.0,
    );
    scene::draw_layout_first_line(
        canvas,
        root,
        &glyph,
        rect.x + (rect.w - glyph.width()) * 0.5,
        rect.y + (rect.h - glyph.height()) * 0.5,
        Some(if stop {
            p.text
        } else if enabled {
            p.on_accent
        } else {
            p.text_dim
        }),
    );
}

fn message_wrap_width(role: ChatRole, width: f32) -> f32 {
    let width = if role == ChatRole::User {
        width * USER_MESSAGE_MAX_FRACTION
    } else {
        width
    };
    (width - PAD * 2.0).max(1.0)
}

fn message_content_width(message: &MessageLayout) -> f32 {
    if !message.attachments.is_empty() {
        let (rects, _) = attachments::card_tiles(&message.attachments, message.wrap_width);
        let tiles = rects.iter().map(|rect| rect.x + rect.w).fold(0.0, f32::max);
        let text = if message.display.trim().is_empty() {
            0.0
        } else {
            message.layout.width()
        };
        return tiles.max(text);
    }
    if message.blocks.is_empty() {
        message.layout.width()
    } else {
        message
            .blocks
            .iter()
            .map(BlockLayout::width)
            .fold(0.0, f32::max)
    }
}

fn message_card(area: Rect, y: f32, h: f32, message: &MessageLayout) -> Rect {
    if message.role != ChatRole::User {
        return Rect::new(area.x, y, area.w, h);
    }
    let max_width = (area.w * USER_MESSAGE_MAX_FRACTION).max(1.0);
    let width =
        (message_content_width(message) + PAD * 2.0).clamp(72.0_f32.min(max_width), max_width);
    Rect::new(area.x + area.w - width, y, width, h)
}

fn tool_activity_action_rects(row: Rect) -> [Rect; 2] {
    let approve_w = 78.0;
    let deny_w = 58.0;
    let y = row.y + row.h - BUTTON_H - 8.0;
    [
        Rect::new(row.x + 10.0, y, approve_w, BUTTON_H),
        Rect::new(row.x + 94.0, y, deny_w, BUTTON_H),
    ]
}

fn tool_activity_title(activity: &ToolActivity) -> String {
    let words = activity.tool_name.replace('_', " ");
    let mut chars = words.chars();
    let title = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Workspace tool".into(),
    };
    let title = activity
        .display_target
        .as_deref()
        .map_or(title.clone(), |target| format!("{title} — {target}"));
    match tool_activity_duration(activity) {
        Some(duration) => format!("{title} - {duration}"),
        None => title,
    }
}

fn tool_activity_duration(activity: &ToolActivity) -> Option<String> {
    let (started, completed) = activity.started_at.zip(activity.completed_at)?;
    let elapsed_ms = completed.millis().checked_sub(started.millis())?;
    if elapsed_ms < 0 {
        return None;
    }
    // Tenths keep fast local tools meaningful without turning the compact row
    // into a stopwatch. Longer tools round to the whole-second form the row is
    // meant to summarize.
    if elapsed_ms >= 10_000 {
        return Some(format!("{}s", elapsed_ms.saturating_add(500) / 1_000));
    }
    let tenths = elapsed_ms.saturating_add(50) / 100;
    Some(if tenths % 10 == 0 {
        format!("{}s", tenths / 10)
    } else {
        format!("{}.{:01}s", tenths / 10, tenths % 10)
    })
}

/// A reply's rows, in the order they happened.
fn reply_rows(transcript: &ChatTranscript, message: MessageId) -> Vec<&ToolActivity> {
    transcript
        .tool_activities
        .iter()
        .filter(|activity| activity.assistant_message_id == message)
        .collect()
}

/// Where a call began in its reply: the stored offset, or the one this
/// surface noted while the reply ran.
fn effective_offset(
    activity: &ToolActivity,
    live: &HashMap<ToolInvocationId, u32>,
) -> Option<usize> {
    activity
        .text_offset
        .or_else(|| live.get(&activity.id).copied())
        .map(|offset| offset as usize)
}

fn tool_activity_detail(activity: &ToolActivity) -> String {
    let access = if activity.read_only { "Read" } else { "Change" };
    let status = match activity.status {
        ToolActivityStatus::Proposed => "proposed",
        ToolActivityStatus::ApprovalRequired => "waiting for approval",
        ToolActivityStatus::Running => "running",
        ToolActivityStatus::Complete => "complete",
        ToolActivityStatus::Denied => "denied",
        ToolActivityStatus::Failed => "failed",
        ToolActivityStatus::Cancelled => "cancelled",
    };
    let mut lines = vec![format!("{access} - {status}")];
    if let Some(target) = &activity.display_target {
        lines.push(format!("Document: {target}"));
    } else if let Some(document) = activity.target_document_id {
        lines.push(format!("Document: {document}"));
    }
    if let Some(message) = &activity.error_message {
        lines.push(format!("Failed: {message}"));
    }
    if let Some(bytes) = activity.result_bytes {
        lines.push(format!("Result: {}", format_result_bytes(bytes)));
    }
    lines.join("\n")
}

fn format_result_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    let kib = bytes as f64 / 1_024.0;
    if kib < 1_024.0 {
        return format!("{kib:.1} KB");
    }
    format!("{:.1} MB", kib / 1_024.0)
}

/// The buttons under a finished reply: copy, branch, store, link. Rating a
/// reply (thumbs up and down) is off for now (the user's call, 2026-11-02);
/// the service keeps its feedback store and API, so it can come back.
const RESPONSE_ACTIONS: usize = 4;

fn response_action_glyph(index: usize) -> char {
    match index {
        0 => icons::COPY,
        1 => icons::GIT_BRANCH,
        2 => icons::FLOPPY_DISK,
        _ => icons::LINK,
    }
}

fn render_chat_block(
    engine: &mut TextEngine,
    id: MessageBlockId,
    markdown: &str,
    max_w: f32,
) -> BlockLayout {
    if let Some(image) = generated_image_spec(markdown) {
        return BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            layout: chat_text_layout(engine, &image.alt, max_w),
            text: image.alt.clone(),
            links: Vec::new(),
            table: None,
            image: Some(image),
        };
    }
    // A bare `https://…` in a reply is a link (D95): the chat follows
    // what it draws as one.
    let styles = koru_markdown::Styles::chat().with_bare_links();
    match koru_markdown::render_block(engine, markdown, max_w, &styles) {
        koru_markdown::Rendered::Text {
            text,
            layout,
            links,
        } => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text,
            layout,
            links,
            table: None,
            image: None,
        },
        koru_markdown::Rendered::Table(table) => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text: String::new(),
            layout: chat_text_layout(engine, "", max_w),
            links: Vec::new(),
            table: Some(table),
            image: None,
        },
        koru_markdown::Rendered::Image { url, alt } => BlockLayout {
            id,
            markdown: markdown.to_owned(),
            wrap_width: max_w,
            text: alt.clone(),
            layout: chat_text_layout(engine, &alt, max_w),
            links: Vec::new(),
            table: None,
            image: Some(ImageSpec { url, alt }),
        },
        koru_markdown::Rendered::Rule => {
            let text = "\u{2500}".repeat(12);
            BlockLayout {
                id,
                markdown: markdown.to_owned(),
                wrap_width: max_w,
                layout: chat_text_layout(engine, &text, max_w),
                text,
                links: Vec::new(),
                table: None,
                image: None,
            }
        }
    }
}

fn generated_image_spec(markdown: &str) -> Option<ImageSpec> {
    let trimmed = markdown.trim();
    let json = if trimmed.starts_with("```") {
        let start = trimmed.find('\n')? + 1;
        let end = trimmed.rfind("```")?;
        trimmed.get(start..end)?.trim()
    } else {
        trimmed
    };
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    (value.get("kind")?.as_str()? == "generated_image").then_some(ImageSpec {
        url: value.get("url")?.as_str()?.to_owned(),
        alt: value
            .get("alt")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("prompt").and_then(serde_json::Value::as_str))
            .unwrap_or("Generated image")
            .to_owned(),
    })
}

/// The image cache's key for a reply picture's URL: the web address
/// itself, or the path of a picture on the disk (`file:` or absolute), as
/// [`ChatSurface::ensure_disk_image`] keeps it.
fn image_key(url: &str) -> String {
    match tool_output::picture::reply_picture_path(url) {
        Some(path) if !is_web_url(url) => path.to_string_lossy().into_owned(),
        _ => url.to_owned(),
    }
}

fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The largest picture file read, from the web or the disk.
const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;

fn load_remote_image(url: &str) -> ImageResult {
    if !is_web_url(url) {
        return Err("image URL is not HTTP or HTTPS".into());
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_BYTES)
    {
        return Err("image is larger than 12 MB".into());
    }
    let bytes = response.bytes().map_err(|error| error.to_string())?;
    decode_image(&bytes)
}

/// **A picture a call read, from its file** (D38), bounded as a reply's
/// picture is. Run off the thread that draws.
fn load_disk_image(path: &std::path::Path) -> ImageResult {
    let size = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_IMAGE_BYTES {
        return Err("image is larger than 12 MB".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    decode_image(&bytes)
}

/// A picture's bytes decoded, made no larger than 1,600 pixels on a side,
/// as the brush it is drawn with.
fn decode_image(bytes: &[u8]) -> ImageResult {
    const MAX_IMAGE_EDGE: u32 = 1_600;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("image is larger than 12 MB".into());
    }
    let mut rgba = image::load_from_memory(bytes)
        .map_err(|error| error.to_string())?
        .to_rgba8();
    let (mut width, mut height) = rgba.dimensions();
    if width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE {
        let scale =
            (MAX_IMAGE_EDGE as f32 / width as f32).min(MAX_IMAGE_EDGE as f32 / height as f32);
        width = (width as f32 * scale).round().max(1.0) as u32;
        height = (height as f32 * scale).round().max(1.0) as u32;
        rgba = image::imageops::resize(&rgba, width, height, image::imageops::FilterType::Lanczos3);
    }
    let data = vello::peniko::ImageData {
        data: vello::peniko::Blob::new(Arc::new(rgba.into_raw())),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width,
        height,
    };
    Ok(LoadedImage {
        brush: vello::peniko::ImageBrush::new(data).with_quality(vello::peniko::ImageQuality::High),
        width,
        height,
    })
}

/// `loaded` drawn as large as fits in `rect`, keeping its shape, centered:
/// a reply's picture in its box, and a call's in a rect already its size.
fn draw_image_fitted(canvas: &mut Scene, root: Affine, loaded: &LoadedImage, rect: Rect) {
    let scale = (rect.w / loaded.width.max(1) as f32).min(rect.h / loaded.height.max(1) as f32);
    let width = loaded.width as f32 * scale;
    let height = loaded.height as f32 * scale;
    let x = rect.x + (rect.w - width) * 0.5;
    let y = rect.y + (rect.h - height) * 0.5;
    let transform = root
        * Affine::translate((x as f64, y as f64))
        * Affine::scale_non_uniform(
            width as f64 / loaded.width.max(1) as f64,
            height as f64 / loaded.height.max(1) as f64,
        );
    canvas.draw_image(&loaded.brush, transform);
}

/// Hand a web link the person clicked to the desktop. `false` when the
/// allow-list refused it.
///
/// **Not on the thread that draws.** The shell call can take as long as the
/// desktop likes — a browser starting cold, an association dialog — and the
/// frame must not wait for it, so it runs on a thread of its own and a
/// failure is logged there; nothing on screen could act on it anyway.
fn open_web_link(url: &str) -> bool {
    if !is_web_url(url) {
        return false;
    }
    #[cfg(test)]
    tests::OPENED.with(|opened| opened.borrow_mut().push(url.to_owned()));
    if cfg!(any(test, feature = "no-client")) {
        return true;
    }
    // **`koru_window::open_uri`, not `webbrowser`.** The workspace dropped that
    // crate deliberately: it launches the default browser with the URL on its
    // *command line*, where this asks the shell to open it. The allow-list
    // above is what makes this call safe, and it stays here because the URL is
    // remote input — it arrives in a model's response.
    let url = url.to_owned();
    let spawned = std::thread::Builder::new()
        .name("koru-ai-open-link".into())
        .spawn(move || {
            // The URL is not logged: it came out of a conversation.
            if let Err(error) = koru_window::open_uri(&url) {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI could not open a web link: {error}"
                );
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(
            target: "koru_ai::ui",
            "AI UI could not start the thread that opens a web link: {error}"
        );
    }
    true
}

fn action_rect(card: Rect, index: usize) -> Rect {
    let gap = 2.0;
    Rect::new(
        card.x + PAD + index as f32 * (ACTION_H + gap),
        card.y + card.h - ACTION_H - 6.0,
        ACTION_H,
        ACTION_H,
    )
}

fn destination_panel(viewport: Rect, count: usize) -> Rect {
    let rows = count.clamp(1, 8);
    let width = (viewport.w - PAD * 4.0).clamp(180.0, 420.0);
    Rect::new(
        viewport.x + (viewport.w - width) * 0.5,
        viewport.y + 70.0,
        width,
        rows as f32 * HISTORY_ROW_H + 8.0,
    )
}

fn destination_row(panel: Rect, index: usize) -> Rect {
    Rect::new(
        panel.x + 4.0,
        panel.y + 4.0 + index as f32 * HISTORY_ROW_H,
        panel.w - 8.0,
        HISTORY_ROW_H,
    )
}

/// The sentence for a failed response, by its class, in the person's
/// language. A class the surface does not name reads as the generic one.
fn failure_label(class: Option<AiErrorClass>, lang: koru_i18n::Lang) -> String {
    let key = match class {
        Some(AiErrorClass::Configuration) => koru_i18n::Key::AiFailureConfiguration,
        Some(AiErrorClass::Authentication) => koru_i18n::Key::AiFailureAuthentication,
        Some(AiErrorClass::RateLimit) => koru_i18n::Key::AiFailureRateLimit,
        Some(AiErrorClass::ContextTooLarge) => koru_i18n::Key::AiFailureContextTooLarge,
        Some(AiErrorClass::ModelUnavailable) => koru_i18n::Key::AiFailureModelUnavailable,
        Some(AiErrorClass::Network) => koru_i18n::Key::AiFailureNetwork,
        Some(AiErrorClass::Cancelled) => koru_i18n::Key::AiFailureCancelled,
        Some(AiErrorClass::Storage) => koru_i18n::Key::AiFailureStorage,
        Some(AiErrorClass::Provider) | None => koru_i18n::Key::AiFailureGeneric,
    };
    koru_i18n::t(lang, key).to_owned()
}

/// The stand-in a new chat is shown as until its first message makes it:
/// what the service answers a create with — its title, no message, no
/// workspace and manual approval — under a fresh id the create will use.
fn unmade_transcript(model: ModelSelection) -> ChatTranscript {
    let id = ChatId::new();
    let now = koru_ai_core::Timestamp::now();
    ChatTranscript {
        chat: Chat {
            id,
            title: DEFAULT_CHAT_TITLE.into(),
            private: false,
            current_model: model,
            branched_from: None,
            agent_working_folder: None,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id: id,
            mode: ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        notices: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        sources: Vec::new(),
        reasoning_control: None,
        agent_commands: None,
        attachments: Vec::new(),
    }
}

/// A service failure as the status line reads it: the sentence its code
/// names, in the person's language, and — when the service said something
/// more specific (*the agent refused that model*) — its own words after
/// it. A code the surface does not know reads as the service's words alone,
/// so nothing a service can say goes unshown.
pub(crate) fn failure_text(failure: &koru_ai_wire::Failure, lang: koru_i18n::Lang) -> String {
    let key = match failure.code.as_str() {
        "ai_configuration" => Some(koru_i18n::Key::AiFailureConfiguration),
        "ai_authentication" => Some(koru_i18n::Key::AiFailureAuthentication),
        "ai_rate_limit" => Some(koru_i18n::Key::AiFailureRateLimit),
        "ai_context_too_large" => Some(koru_i18n::Key::AiFailureContextTooLarge),
        "ai_model_unavailable" => Some(koru_i18n::Key::AiFailureModelUnavailable),
        "ai_network" => Some(koru_i18n::Key::AiFailureNetwork),
        "ai_provider" => Some(koru_i18n::Key::AiFailureProvider),
        "ai_cancelled" => Some(koru_i18n::Key::AiFailureCancelled),
        "ai_storage" => Some(koru_i18n::Key::AiFailureStorage),
        "not_found" => Some(koru_i18n::Key::AiFailureNotFound),
        _ => None,
    };
    let message = failure.message.trim().trim_end_matches('.');
    match key {
        Some(key) => {
            let sentence = koru_i18n::t(lang, key);
            if message.is_empty() || message.eq_ignore_ascii_case(sentence) {
                sentence.to_owned()
            } else {
                format!("{sentence} · {message}")
            }
        }
        None => message.to_owned(),
    }
}

fn choices_for_reasoning(control: &ReasoningControl) -> Vec<ReasoningChoice> {
    match control {
        ReasoningControl::Unsupported => Vec::new(),
        ReasoningControl::Toggle { .. } => vec![
            ReasoningChoice::Default,
            ReasoningChoice::Off,
            ReasoningChoice::On,
        ],
        ReasoningControl::Levels {
            values,
            supports_off,
            ..
        } => {
            let mut choices = vec![ReasoningChoice::Default];
            if *supports_off {
                choices.push(ReasoningChoice::Off);
            }
            choices.extend(values.iter().copied().map(ReasoningChoice::Level));
            choices
        }
    }
}

#[cfg(test)]
fn provider_ready(settings: &ProviderSettingsResponse) -> bool {
    settings
        .selected_provider
        .as_ref()
        .is_some_and(|provider| match provider.as_str() {
            AiProviderId::OPENAI => {
                settings.openai.credential_configured && settings.openai.model.is_some()
            }
            AiProviderId::OLLAMA => settings.ollama.model.is_some(),
            AiProviderId::KORU => true,
            _ => false,
        })
}

fn selected_setup_provider(settings: &ProviderSettingsResponse) -> SetupProvider {
    match settings
        .selected_provider
        .as_ref()
        .map(AiProviderId::as_str)
    {
        Some(AiProviderId::OLLAMA) => SetupProvider::Ollama,
        Some(AiProviderId::KORU) => SetupProvider::Koru,
        Some(provider) if agents::is_agent_provider_str(provider) => SetupProvider::Agents,
        _ => SetupProvider::OpenAi,
    }
}

/// A chat's name from its first message (the review's note): the first line
/// that says something, its Markdown marks stripped, cut at a word before
/// `AUTO_TITLE_CHARS` with an ellipsis. `None` when nothing is left.
fn title_from_message(markdown: &str) -> Option<String> {
    let line = markdown
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(|c: char| matches!(c, '#' | '>' | '-' | '*' | '`'))
                .trim()
        })
        .find(|line| !line.is_empty())?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut title = String::new();
    for word in &words {
        let next = if title.is_empty() {
            (*word).to_owned()
        } else {
            format!("{title} {word}")
        };
        if next.chars().count() > AUTO_TITLE_CHARS {
            break;
        }
        title = next;
    }
    if title.is_empty() {
        title = line.chars().take(AUTO_TITLE_CHARS).collect();
    }
    let cut = title.chars().count() < line.chars().count();
    if cut {
        title.push('…');
    }
    Some(title)
}

fn validation_status_text(status: ProviderValidationStatus) -> &'static str {
    match status {
        ProviderValidationStatus::Unconfigured => "Finish configuring this provider.",
        ProviderValidationStatus::Unavailable => "The provider is not reachable.",
        ProviderValidationStatus::NoModels => "No installed models were found.",
        ProviderValidationStatus::Ready => "Provider ready.",
        ProviderValidationStatus::Failed => "Provider validation failed.",
    }
}

fn likely_openai_text_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    (model.starts_with("gpt-")
        || model.starts_with("chatgpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4"))
        && ![
            "audio",
            "realtime",
            "transcribe",
            "tts",
            "image",
            "embedding",
            "moderation",
        ]
        .iter()
        .any(|excluded| model.contains(excluded))
}

fn sort_model_options(models: &mut [String]) {
    models.sort_by(|a, b| {
        let family = |model: &str| (!model.starts_with("gpt-"), !model.starts_with('o'));
        family(a).cmp(&family(b)).then_with(|| b.cmp(a))
    });
}

fn preferred_openai_model(models: &[String]) -> Option<&str> {
    models
        .iter()
        .filter(|model| likely_openai_text_model(model))
        .max()
        .or_else(|| models.first())
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_ai_core::ReasoningLevel;
    use std::time::Duration;

    thread_local! {
        /// Every web address this thread's surface handed the desktop to
        /// open, which under test opens nothing.
        pub(crate) static OPENED: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// Every link a card copied, which under test leaves the system
        /// clipboard alone.
        pub(crate) static COPIED: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    use koru_ai_core::{ProfileId, ToolApproval, UsageTotals};

    fn transcript() -> ChatTranscript {
        let now = koru_ai_core::Timestamp::from_millis(1);
        let chat_id = ChatId::new();
        ChatTranscript {
            sources: Vec::new(),
            chat: Chat {
                id: chat_id,
                title: "Test chat".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            notices: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
            agent_commands: None,
            attachments: Vec::new(),
        }
    }

    fn destination(workspace_id: WorkspaceId, label: &str) -> DestinationOption {
        DestinationOption {
            profile_id: ProfileId::new(),
            workspace_id,
            label: label.into(),
            name: label.rsplit(" / ").next().unwrap_or(label).into(),
        }
    }

    #[test]
    fn current_chat_id_follows_a_link_before_the_transcript_loads() {
        let mut surface = ChatSurface::new();
        let id = ChatId::new();
        assert!(surface.current_chat_id().is_none());
        assert!(surface.open_link(id, None));
        assert_eq!(surface.current_chat_id(), Some(id));
    }

    #[test]
    fn drain_updates_is_a_no_op_without_a_reader() {
        let mut surface = ChatSurface::new();
        assert!(!surface.drain_updates());
        assert!(surface.listing_deadline().is_none());
    }

    #[test]
    fn recent_chats_takes_the_first_n_in_list_order() {
        let mut surface = ChatSurface::new();
        let a = ChatId::new();
        let b = ChatId::new();
        let chat = |id, title: &str| Chat {
            id,
            title: title.into(),
            ..transcript().chat
        };
        surface.chats = vec![chat(a, "One"), chat(b, "Two")];
        assert_eq!(surface.recent_chats(1), vec![(a, "One".into())]);
        assert_eq!(
            surface.recent_chats(10),
            vec![(a, "One".into()), (b, "Two".into())]
        );
    }

    #[test]
    fn test_surface_starts_no_reader_or_maintenance_clock() {
        let surface = ChatSurface::new();
        assert!(surface.reader.is_none());
        assert!(surface.maintenance_deadline().is_none());
    }

    #[test]
    fn workspace_file_choice_retries_import_and_copy_with_one_operation() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse("agent-claude").unwrap();
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        let workspace = WorkspaceId::new();
        surface.context_workspace = Some(workspace);

        assert!(
            surface
                .computer_path_chosen(ComputerPickerKind::File, PathBuf::from("C:/fixture.pdf"),)
        );
        assert!(surface.finish_computer_file_choice(ChoiceOutcome::Chosen(0)));
        let Command::AddGrants(granted_chat, Some(granted_workspace), _, op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("grant must precede import");
        };
        assert_eq!((granted_chat, granted_workspace), (chat_id, workspace));
        let draft = &surface.strips.get(chat_id)[0];
        let id = draft.id;
        assert_eq!(draft.state, attachments::DraftState::Storing);
        assert!(surface.take_workspace_import_request().is_none());

        let policy = koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: vec![workspace],
        };
        surface.apply_update(Update::GrantsAdded {
            chat: chat_id,
            op: uuid::Uuid::new_v4(),
            result: Ok(policy.clone()),
        });
        assert!(surface.take_workspace_import_request().is_none());
        surface.apply_update(Update::GrantsAdded {
            chat: chat_id,
            op,
            result: Ok(policy.clone()),
        });
        let first = surface.take_workspace_import_request().unwrap();
        assert_eq!((first.op, first.attachment), (op, id));
        assert!(surface.workspace_file_imported(op, Err("transient import failure".into())));
        assert_eq!(
            surface.strips.get(chat_id)[0].state,
            attachments::DraftState::Failed { retryable: true }
        );

        assert!(surface.retry_workspace_import(chat_id, id));
        let Command::AddGrants(_, _, _, retry_op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("retry must recheck the grant");
        };
        assert_eq!(retry_op, op);
        surface.apply_update(Update::GrantsAdded {
            chat: chat_id,
            op,
            result: Ok(policy.clone()),
        });
        let retry = surface.take_workspace_import_request().unwrap();
        assert_eq!((retry.op, retry.attachment), (op, id));

        let document = DocumentId::new();
        assert!(surface.workspace_file_imported(op, Ok(document)));
        let Command::CopyWorkspaceFile(copied_chat, copy) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("copy must follow import");
        };
        assert_eq!(
            (copied_chat, copy.id, copy.document),
            (chat_id, id, document)
        );
        surface.apply_update(Update::AttachmentCopied {
            chat: chat_id,
            id,
            result: Err(koru_ai_wire::Failure {
                code: "workspace_file_unavailable".into(),
                message: "copy failed".into(),
                retryable: true,
                details: Default::default(),
            }),
        });
        assert_eq!(
            surface.strips.get(chat_id)[0].state,
            attachments::DraftState::Failed { retryable: true }
        );

        assert!(surface.retry_workspace_import(chat_id, id));
        let Command::AddGrants(_, _, _, retry_op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("retry must recheck the grant");
        };
        assert_eq!(retry_op, op);
        surface.apply_update(Update::GrantsAdded {
            chat: chat_id,
            op,
            result: Ok(policy),
        });
        assert!(
            surface.take_workspace_import_request().is_none(),
            "known document skips import"
        );
        let Command::CopyWorkspaceFile(_, copy_again) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("retry should copy the known file");
        };
        assert_eq!(copy_again, copy);

        surface.apply_update(Update::AttachmentCopied {
            chat: chat_id,
            id,
            result: Ok(koru_ai_core::ChatAttachment {
                id,
                message_id: None,
                ordinal: None,
                kind: koru_ai_core::AttachmentKind::File,
                source: koru_ai_core::AttachmentSource::Workspace,
                file_name: "fixture.pdf".into(),
                media_type: "application/pdf".into(),
                byte_count: 12,
                dimensions: None,
                origin: Some(koru_ai_core::WorkspaceFileOrigin {
                    workspace_id: workspace,
                    document_id: document,
                }),
                reference_path: None,
                created_at: koru_ai_core::Timestamp::now(),
            }),
        });
        assert_eq!(
            surface.strips.get(chat_id)[0].state,
            attachments::DraftState::Stored
        );
        assert_eq!(
            surface.composer.message().documents,
            vec![(document, "fixture.pdf".into())]
        );
    }

    #[test]
    fn plus_menu_opens_file_and_working_folder_pickers_directly() {
        let mut surface = ChatSurface::new();
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse("agent-claude").unwrap();
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        surface.composer_rect();

        surface.open_plus_menu();
        assert_eq!(
            surface
                .plus_menu
                .as_ref()
                .unwrap()
                .panel(surface.menu_viewport())
                .w,
            430.0
        );
        let items = surface.plus_menu.as_ref().unwrap().items();
        assert!(
            items
                .iter()
                .all(|item| !koru_ui::menu::MenuItem::has_submenu(item)
                    || matches!(item, composer_plus::PlusItem::Recent(_)))
        );
        let file = items
            .iter()
            .position(|item| matches!(item, composer_plus::PlusItem::UploadFiles(_)))
            .unwrap();
        assert!(surface.choose_plus_item(file));
        assert_eq!(
            surface.take_computer_picker_request(),
            Some(ComputerPickerKind::File)
        );

        surface.open_plus_menu();
        let folder = surface
            .plus_menu
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .position(|item| matches!(item, composer_plus::PlusItem::WorkInProject(..)))
            .unwrap();
        assert!(surface.choose_plus_item(folder));
        assert_eq!(
            surface.take_working_folder_request(),
            Some(WorkingFolderRequest {
                chat: chat_id,
                start: None
            })
        );
        assert!(surface.take_computer_picker_request().is_none());
    }

    #[test]
    fn plus_project_submenu_imports_to_the_current_workspace() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse("agent-claude").unwrap();
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        let workspace = WorkspaceId::new();
        surface.context_workspace = Some(workspace);
        surface.composer_rect();

        surface.open_plus_menu();
        let project = surface
            .plus_menu
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .position(|item| matches!(item, composer_plus::PlusItem::AddToProject(_)))
            .unwrap();
        assert!(surface.choose_plus_item(project));
        assert!(matches!(
            surface.plus_menu.as_ref().unwrap().items()[1],
            composer_plus::PlusItem::ImportToWorkspace(_)
        ));
        assert!(surface.choose_plus_item(1));
        assert_eq!(
            surface.take_computer_picker_request(),
            Some(ComputerPickerKind::WorkspaceFile)
        );
        assert!(surface.computer_path_chosen(
            ComputerPickerKind::WorkspaceFile,
            PathBuf::from("C:/fixture.pdf"),
        ));
        assert!(surface.computer_file_choice.is_none());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::AddGrants(chat, Some(granted), _, _) if chat == chat_id && granted == workspace
        ));
    }

    #[test]
    fn folder_reference_is_attachment_only_and_recall_mints_a_fresh_path_row() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse("agent-claude").unwrap();
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        let path = PathBuf::from("C:/Project/日本語 folder");
        assert!(surface.computer_path_chosen(ComputerPickerKind::Folder, path.clone()));
        let source_id = surface.strips.get(chat_id)[0].id;
        assert_eq!(
            surface.strips.get(chat_id)[0].reference_path.as_deref(),
            Some(path.to_str().unwrap())
        );
        surface.on_stored(
            chat_id,
            source_id,
            Ok(koru_ai_core::ChatAttachment {
                id: source_id,
                message_id: None,
                ordinal: None,
                kind: koru_ai_core::AttachmentKind::Folder,
                source: koru_ai_core::AttachmentSource::Path,
                file_name: "日本語 folder".into(),
                media_type: "inode/directory".into(),
                byte_count: 0,
                dimensions: None,
                origin: None,
                reference_path: Some(path.to_string_lossy().into_owned()),
                created_at: koru_ai_core::Timestamp::now(),
            }),
        );
        assert!(surface.send_composer());
        assert!(commands
            .try_iter()
            .any(|command| matches!(command, Command::Send(request) if request.attachment_ids == vec![source_id])));
        let recalled = surface.sent_inputs.last().unwrap().attachments.clone();
        assert_eq!(
            recalled[0].reference_path.as_deref(),
            Some(path.to_str().unwrap())
        );
        surface.replace_recalled_attachments(chat_id, &recalled);
        let fresh = &surface.strips.get(chat_id)[0];
        assert_ne!(fresh.id, source_id);
        assert_eq!(fresh.source, koru_ai_core::AttachmentSource::Path);
        assert_eq!(
            fresh.reference_path.as_deref(),
            Some(path.to_str().unwrap())
        );
    }

    #[test]
    fn dismiss_popovers_closes_every_transient_overlay() {
        let mut surface = ChatSurface::new();
        surface.chat_model_open = true;
        surface.tool_access_open = true;
        surface.tool_mode_open = true;
        surface.model_options_open = true;
        surface.destination_open = Some(MessageId::new());
        surface.response_menu = Some(Menu::new(&mut surface.engine, (0.0, 0.0), vec![]));
        surface.response_menu_anchor = Some(Rect::new(0.0, 0.0, 0.0, 0.0));
        surface.confirm = Some(Confirm::new("", "", ""));

        assert!(surface.dismiss_popovers());
        assert!(!surface.chat_model_open);
        assert!(!surface.tool_access_open);
        assert!(!surface.tool_mode_open);
        assert!(!surface.model_options_open);
        assert!(surface.destination_open.is_none());
        assert!(surface.response_menu.is_none());
        assert!(surface.response_menu_anchor.is_none());
        assert!(surface.confirm.is_none());

        // Calling again is a no-op.
        assert!(!surface.dismiss_popovers());
    }

    #[test]
    fn context_document_attaches_with_title_and_updates_title_without_dismissal() {
        let mut surface = ChatSurface::new();
        let doc_id = DocumentId::new();
        assert!(surface.set_context_document(Some((doc_id, "Launch plan".into()))));
        assert_eq!(
            surface.context_document,
            Some((doc_id, "Launch plan".into()))
        );

        // Title update alone does not re-attach or touch dismissal.
        assert!(surface.set_context_document(Some((doc_id, "Launch plan v2".into()))));
        assert_eq!(
            surface.context_document,
            Some((doc_id, "Launch plan v2".into()))
        );
        assert!(surface.dismissed_context_document.is_none());
    }

    #[test]
    fn removing_context_document_dismisses_it_until_document_changes() {
        let mut surface = ChatSurface::new();
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));
        surface.remove_context_document();
        assert!(surface.context_document.is_none());
        assert_eq!(surface.dismissed_context_document, Some(doc_a));

        // Same document stays dismissed.
        assert!(!surface.set_context_document(Some((doc_a, "A updated".into()))));
        assert!(surface.context_document.is_none());

        // A different document clears dismissal and attaches.
        let doc_b = DocumentId::new();
        assert!(surface.set_context_document(Some((doc_b, "B".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_b));
        assert!(surface.dismissed_context_document.is_none());
    }

    #[test]
    fn a_tab_without_a_document_clears_the_pill_without_recording_a_dismissal() {
        let mut surface = ChatSurface::new();
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));

        // The new tab has no document; the previous tab's pill must not linger
        // (and must not be sent as context).
        assert!(surface.set_context_document(None));
        assert!(surface.context_document.is_none());

        // Nothing was dismissed, so returning to the document re-offers it.
        assert!(surface.set_context_document(Some((doc_a, "A".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_a));
    }

    #[test]
    fn clearing_dismissal_re_attaches_the_current_document() {
        let mut surface = ChatSurface::new();
        let doc_a = DocumentId::new();
        surface.set_context_document(Some((doc_a, "A".into())));
        surface.remove_context_document();
        assert!(surface.context_document.is_none());

        surface.clear_context_document_dismissal();
        assert!(surface.set_context_document(Some((doc_a, "A".into()))));
        assert_eq!(surface.context_document.map(|(id, _)| id), Some(doc_a));
    }

    #[test]
    fn send_composer_omits_context_document_after_removal() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript.clone());
        surface.composer.set_text("hello");

        let doc_id = DocumentId::new();
        surface.set_context_document(Some((doc_id, "Launch plan".into())));
        surface.remove_context_document();

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert!(request.explicit_refs.is_empty()),
            _ => panic!("composer send must issue a turn"),
        }
    }

    #[test]
    fn agent_send_waits_for_its_matching_workspace_grant() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        let chat = transcript.chat.id;
        let workspace = WorkspaceId::new();
        let mut policy = transcript.tool_policy.clone();
        policy.workspace_ids = vec![workspace];
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.context_workspace = Some(workspace);
        surface.composer.set_text("Ship this");

        assert!(surface.send_composer());
        let Command::AddGrants(actual_chat, current, documents, op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("the grant must precede the turn");
        };
        assert_eq!(actual_chat, chat);
        assert_eq!(current, Some(workspace));
        assert!(documents.is_empty());
        assert!(commands.try_recv().is_err());
        assert_eq!(surface.composer.text(), "Ship this");

        surface.apply_update(Update::GrantsAdded {
            chat,
            op: uuid::Uuid::new_v4(),
            result: Ok(policy.clone()),
        });
        assert!(commands.try_recv().is_err(), "an unrelated ack cannot send");
        assert_eq!(surface.composer.text(), "Ship this");

        surface.apply_update(Update::GrantsAdded {
            chat,
            op,
            result: Ok(policy),
        });
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Send(_)
        ));
    }

    #[test]
    fn task_node_link_grants_its_containing_document_before_send() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        let chat = transcript.chat.id;
        let policy = transcript.tool_policy.clone();
        let document = DocumentId::new();
        let node = koru_core::NodeId::new();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.document_titles.insert(document, "Plan".into());
        surface
            .composer
            .set_message(&format!("[[koru://doc/{document}#{node}]]"), |_| {
                "Ship café".into()
            });
        assert!(surface.send_composer());
        let Command::AddGrants(actual, _, documents, op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("document grant precedes task send");
        };
        assert_eq!(actual, chat);
        assert_eq!(documents, vec![document]);
        assert!(commands.try_recv().is_err());
        surface.apply_update(Update::GrantsAdded {
            chat,
            op,
            result: Ok(policy),
        });
        let Command::Send(request) = commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("task turn follows grant acknowledgement");
        };
        assert_eq!(
            request.explicit_refs,
            vec![ContextRef::Document {
                document_id: document
            }]
        );
        assert_eq!(surface.document_titles.get(&document).unwrap(), "Plan");
    }

    #[test]
    fn refused_grant_keeps_the_draft_and_never_queues_send() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        let chat = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.context_workspace = Some(WorkspaceId::new());
        surface.composer.set_text("Keep this draft");
        assert!(surface.send_composer());
        let Command::AddGrants(_, _, _, op) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("grant expected");
        };
        surface.apply_update(Update::GrantsAdded {
            chat,
            op,
            result: Err("A workspace is not in this chat's profile.".into()),
        });
        assert_eq!(surface.composer.text(), "Keep this draft");
        assert!(commands.try_recv().is_err());
        assert_eq!(
            surface.status.as_deref(),
            Some("A workspace is not in this chat's profile.")
        );
        assert!(!surface.waiting);
    }

    #[test]
    fn a_grant_ack_does_not_send_attachments_added_while_it_was_pending() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        let chat = transcript.chat.id;
        let mut policy = transcript.tool_policy.clone();
        let workspace = WorkspaceId::new();
        policy.workspace_ids = vec![workspace];
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.context_workspace = Some(workspace);
        surface.composer.set_text("Keep the revised draft");
        assert!(surface.send_composer());
        let Command::AddGrants(_, _, _, op) = commands.try_recv().unwrap() else {
            panic!("grant expected");
        };
        surface.strips.push(
            chat,
            attachments::Draft::from_path(
                std::path::PathBuf::from("notes.pdf"),
                koru_ai_core::AttachmentSource::FilePicker,
            ),
        );
        surface.apply_update(Update::GrantsAdded {
            chat,
            op,
            result: Ok(policy),
        });
        assert!(commands.try_recv().is_err());
        assert_eq!(surface.composer.text(), "Keep the revised draft");
        assert_eq!(surface.strip().len(), 1);
    }

    #[test]
    fn context_pane_without_document_sends_immediately() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.composer.set_text("quick test");

        assert!(surface.send_composer());
        assert!(surface.staged_embedded_send.is_none());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.markdown, "quick test");
                assert!(request.explicit_refs.is_empty());
            }
            _ => panic!("composer send must issue a turn"),
        }
    }

    #[test]
    fn context_pane_with_document_stages_until_durable() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.composer.set_text("document question");

        let doc_id = DocumentId::new();
        surface.set_context_document(Some((doc_id, "Notes".into())));

        assert!(surface.send_composer());
        assert!(surface.staged_embedded_send.is_some());
        assert!(commands.try_recv().is_err());

        // First durable observation is not enough (needs 2 observations).
        assert!(!surface.submit_embedded_when_durable(true));
        assert!(surface.staged_embedded_send.is_some());
        assert!(commands.try_recv().is_err());

        // Second durable observation releases and submits the send.
        assert!(surface.submit_embedded_when_durable(true));
        assert!(surface.staged_embedded_send.is_none());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.markdown, "document question");
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: doc_id
                    }]
                );
            }
            _ => panic!("composer send must issue a turn"),
        }
    }

    /// **New chat opens a new chat, not the last one.** The boot is the
    /// first answer a fresh tab gets, and it carries the service's current
    /// chat; a tab that asked for a new chat drops it there too.
    #[test]
    fn a_new_chat_tab_does_not_take_the_last_chat_from_the_boot() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat());
        let last = transcript();
        surface.apply_update(Update::Boot {
            settings: ProviderSettingsResponse {
                selected_provider: Some(AiProviderId::parse("openai").unwrap()),
                openai: koru_ai_wire::OpenAiSettingsView {
                    model: Some("gpt-5.1".into()),
                    reasoning: ReasoningChoice::Default,
                    credential_configured: true,
                },
                ollama: koru_ai_wire::OllamaSettingsView {
                    endpoint: "http://localhost:11434".into(),
                    model: None,
                    reasoning: ReasoningChoice::Default,
                },
                agents: Vec::new(),
            },
            chats: vec![last.chat.clone()],
            transcript: Some(last.clone()),
            link_missing: false,
            destinations: Vec::new(),
            destination_error: None,
        });
        assert!(
            surface.transcript.is_none(),
            "the last chat is not this tab's"
        );
        assert!(surface.fresh_chat_wanted, "the new chat is still owed");
        assert_eq!(surface.chats.len(), 1, "the list is still the list");
    }

    /// **A tab reopened on a chat that has since been deleted becomes a new
    /// chat** (ADR-483-chattabrestore): the boot says the chat it was asked
    /// for is missing, and the tab forgets it rather than naming it — so it
    /// is not saved over it again — and owes a new chat instead.
    #[test]
    fn a_tab_opened_on_a_deleted_chat_becomes_a_new_chat() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        let gone = ChatId::new();
        assert!(surface.open_link(gone, None));
        surface.reader = Some(reader);
        assert_eq!(surface.current_chat_id(), Some(gone), "opening it");
        let other = transcript();
        surface.apply_update(Update::Boot {
            settings: unchosen_settings(&[]),
            chats: vec![other.chat.clone()],
            transcript: None,
            link_missing: true,
            destinations: Vec::new(),
            destination_error: None,
        });
        assert_eq!(
            surface.current_chat_id(),
            None,
            "the gone chat is forgotten"
        );
        assert!(surface.transcript.is_none(), "no other chat is taken");
        assert!(surface.fresh_chat_wanted, "a new chat is owed");
        assert!(surface.settings.is_some(), "and the boot completed");
        assert!(!surface.current_chat_made());
    }

    /// **A tab opened for a new chat gets one** (the review's note): the boot
    /// answers with the service's current chat, and before that answer there
    /// is no provider to create under. The intent is kept, the snapshot's
    /// chat is dropped, and the chat this tab asked for is the one it shows.
    #[test]
    fn a_tab_asked_for_a_new_chat_does_not_take_the_services_current_one() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat(), "the intent is kept");
        assert!(
            commands.try_recv().is_err(),
            "nothing to create under before the boot answers"
        );
        assert!(!surface.setup_open, "and no setup is forced open");
        let existing = transcript();
        surface.apply_update(Update::HistoryChanged {
            chats: vec![existing.chat.clone()],
            transcript: Some(existing.clone()),
        });
        assert!(
            surface.transcript.is_none(),
            "the service's current chat is not this tab's"
        );
        assert_eq!(surface.chats.len(), 1, "the list is still the list");
        let mut fresh = transcript();
        fresh.chat.id = ChatId::new();
        surface.apply_update(Update::Created(fresh.clone()));
        assert_eq!(
            surface.transcript.as_ref().map(|t| t.chat.id),
            Some(fresh.chat.id)
        );
        assert!(!surface.fresh_chat_wanted, "the debt is paid");
        surface.apply_update(Update::HistoryChanged {
            chats: vec![existing.chat.clone()],
            transcript: Some(existing.clone()),
        });
        assert_eq!(
            surface.transcript.as_ref().map(|t| t.chat.id),
            Some(existing.chat.id),
            "a later snapshot applies as it always did"
        );
    }

    /// Settings where nothing was ever chosen: no provider, no OpenAI key,
    /// no Ollama model, and the agents named.
    fn unchosen_settings(agents: &[&str]) -> ProviderSettingsResponse {
        ProviderSettingsResponse {
            selected_provider: None,
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: agents
                .iter()
                .map(|id| koru_ai_wire::AgentProviderView {
                    id: AiProviderId::parse(*id).unwrap(),
                    label: "Claude".into(),
                    steers: false,
                })
                .collect(),
        }
    }

    fn boot(surface: &mut ChatSurface, settings: ProviderSettingsResponse) {
        surface.apply_update(Update::Boot {
            settings,
            chats: Vec::new(),
            transcript: None,
            link_missing: false,
            destinations: Vec::new(),
            destination_error: None,
        });
    }

    /// **Nothing opens the provider setup**, for now: not the boot with no
    /// provider chosen, not a settings answer, not a new chat with nothing
    /// to make it with.
    #[test]
    fn no_door_opens_the_provider_setup() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat());
        boot(&mut surface, unchosen_settings(&[]));
        assert!(!surface.setup_open, "the boot opens the chat view");
        assert!(!surface.showing_setup(), "no ready provider is no setup");
        surface.apply_update(Update::Settings(unchosen_settings(&[])));
        assert!(!surface.showing_setup(), "nor does a settings answer");
        assert!(surface.create_chat());
        assert!(!surface.showing_setup(), "nor a chat with no model");
        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::Unavailable,
            models: Vec::new(),
            selected_model: None,
            failure: None,
        }));
        assert!(!surface.showing_setup(), "nor a provider that is down");
    }

    /// **A new chat with no model ever chosen is the chat view**: the
    /// greeting and the composer, the chip asking for a model, send off.
    /// Enter opens the picker instead of sending, and the message waits.
    #[test]
    fn a_fresh_chat_with_no_model_waits_on_the_chip() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat());
        boot(&mut surface, unchosen_settings(&[]));
        while commands.try_recv().is_ok() {}
        assert!(surface.transcript.is_none());
        assert!(surface.awaiting_model());
        assert!(surface.greeting().is_some(), "the greeting shows");
        assert_eq!(
            surface.chip_label(),
            koru_i18n::t(surface.lang, koru_i18n::Key::AiChooseModel),
            "the chip asks for a model"
        );
        assert_eq!(
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::AiChooseModel),
            "Choose a model"
        );

        surface.composer.set_text("Plan the launch");
        surface.composer_rect();
        let send = surface.send_button();
        assert!(surface.mouse_down(send.x + send.w / 2.0, send.y + send.h / 2.0));
        assert!(!surface.chat_model_open, "the send button is off");
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        assert!(surface.chat_model_open, "Enter opens the picker");
        while let Ok(command) = commands.try_recv() {
            assert!(
                !matches!(command, Command::Send(_) | Command::Create(_)),
                "nothing is sent or made before a model is chosen"
            );
        }
        assert_eq!(surface.composer.text(), "Plan the launch", "it waits");
    }

    /// The picker of a chat with no model offers only the local agents:
    /// Koru AI, OpenAI and Ollama are not on its rail, a saved OpenAI key or
    /// not. A chat that has begun on one keeps it alone, the rail hidden
    /// (S1; the user: *we don't support switching models*).
    #[test]
    fn the_picker_of_a_chat_with_no_model_offers_only_the_agents() {
        let mut surface = ChatSurface::new();
        surface.settings = Some(unchosen_settings(&["agent-claude"]));
        surface
            .settings
            .as_mut()
            .unwrap()
            .openai
            .credential_configured = true;
        let claude = AiProviderId::parse("agent-claude").unwrap();
        assert_eq!(
            surface.picker_rail(),
            vec![model_picker::RailEntry::Agent(claude)]
        );
        // A chat that has begun on OpenAI lists its own provider's models.
        let mut begun = transcript();
        begun.messages.push(koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: begun.chat.id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "hi".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
        });
        surface.transcript = Some(begun);
        surface.openai_models = vec!["gpt-test".into()];
        let openai = AiProviderId::parse(AiProviderId::OPENAI).unwrap();
        assert_eq!(
            surface.picker_rail(),
            vec![model_picker::RailEntry::Provider(openai.clone())],
            "not Claude: the chat keeps its provider"
        );
        assert!(surface.picker_rail_hidden());
        assert_eq!(
            surface.picker_initial_entry(),
            Some(model_picker::RailEntry::Provider(openai))
        );
        assert_eq!(
            surface.picker_rows(),
            vec![model_picker::Row::Model("gpt-test".into())]
        );
    }

    /// The surface of a chat not made yet, on Claude: a reader, settings with
    /// the agent ready, and the pick made in the composer.
    fn unmade_claude_chat() -> (
        ChatSurface,
        std::sync::mpsc::Receiver<Command>,
        std::sync::mpsc::SyncSender<Update>,
    ) {
        let mut surface = ChatSurface::new();
        let (reader, commands, updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.settings = Some(unchosen_settings(&["agent-claude"]));
        let claude = AiProviderId::parse("agent-claude").unwrap();
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::Ready);
        assert!(surface.choose_agent_model(claude, AGENT_DEFAULT_MODEL.into()));
        (surface, commands, updates)
    }

    /// **Picking a model makes no chat**: the pick is kept on the chat on
    /// screen, the chip names it, and it is remembered so the next chat opens
    /// on it. Nothing reaches the Chats list until the first message.
    #[test]
    fn picking_an_agent_with_no_chat_waits_for_the_first_message() {
        let (surface, commands, _updates) = unmade_claude_chat();
        let claude = AiProviderId::parse("agent-claude").unwrap();
        let selected = loop {
            match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
                Command::DiscoverCommands(..) => continue,
                Command::SelectProvider(selected) => break selected,
                _ => panic!("the choice is remembered, and nothing is made"),
            }
        };
        assert_eq!(selected.provider, claude);
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok())
                .all(|command| matches!(command, Command::DiscoverCommands(..))),
            "no chat is made by a pick"
        );
        assert!(surface.chat_unmade());
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .map(|t| t.chat.current_model.provider.clone()),
            Some(claude)
        );
        assert_eq!(surface.chip_label(), "Claude", "the chip names the pick");
        assert!(!surface.awaiting_model(), "send is on");
        assert_eq!(surface.current_chat_id(), None, "no chat a host can name");
        assert!(surface.chats.is_empty(), "nothing in the Chats list");
        assert!(!surface.showing_setup());
    }

    /// **What a chat tab wears is read here** (D22): a new chat is not made
    /// until its first message, so its tab stays *AI Chat*; once sent, the
    /// chat is made and names who answers it, and the agent's id maps to its
    /// brand through the one mapping the model picker draws with.
    #[test]
    fn a_chat_is_made_by_its_first_message_and_names_its_agent() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        assert!(!surface.current_chat_made(), "not sent, not made");
        assert_eq!(surface.current_chat_provider(), Some("agent-claude"));
        surface.composer.set_text("Plan the launch");
        assert!(surface.send_composer());
        while commands.try_recv().is_ok() {}
        assert!(surface.current_chat_made(), "the first message made it");
        assert_eq!(
            surface.current_chat_own_title(),
            None,
            "the placeholder is not a name; the automatic title is on its way"
        );
        if let Some(transcript) = surface.transcript.as_mut() {
            transcript.chat.title = "Plan the launch".into();
        }
        assert_eq!(surface.current_chat_own_title(), Some("Plan the launch"));
        assert_eq!(
            surface.current_chat_provider().and_then(provider_mark),
            Some(koru_draw::brand::ProfileMark::Brand(
                &koru_draw::brand::CLAUDE
            ))
        );
    }

    /// **The first message makes the chat**, then goes into it: the create is
    /// queued ahead of the message, under the id the message names, and the
    /// message's card stays through the chat's arrival.
    #[test]
    fn the_first_message_makes_the_chat_and_is_sent_into_it() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        while commands.try_recv().is_ok() {}
        let stand_in = surface.transcript.clone().unwrap();
        surface.composer.set_text("Plan the launch");
        assert!(surface.send_composer());
        let Command::Create(create) = commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("the chat is made first");
        };
        assert_eq!(create.chat_id, stand_in.chat.id);
        assert_eq!(create.model, stand_in.chat.current_model);
        assert_eq!(create.title, DEFAULT_CHAT_TITLE);
        let Command::Send(send) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("then the message goes into it");
        };
        assert_eq!(send.chat_id, stand_in.chat.id);
        assert_eq!(send.markdown, "Plan the launch");
        assert!(surface.composer.is_blank());
        assert!(!surface.chat_unmade());
        assert_eq!(surface.current_chat_id(), Some(stand_in.chat.id));

        let mut made = stand_in.clone();
        made.chat.created_at = koru_ai_core::Timestamp::from_millis(7);
        surface.apply_update(Update::Created(made));
        assert_eq!(
            surface.pending_user.as_deref(),
            Some("Plan the launch"),
            "the message's card stays while it is on its way"
        );
        assert!(surface.waiting, "and the surface keeps listening for it");
        assert_eq!(surface.chats.len(), 1, "the chat is listed once it is made");
    }

    /// **A failed create gives the message back**, with the create's reason:
    /// the chat on screen is a stand-in again, and the next send asks for it
    /// again.
    #[test]
    fn a_failed_create_gives_the_first_message_back() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        while commands.try_recv().is_ok() {}
        let chat = surface.transcript.as_ref().unwrap().chat.id;
        surface.composer.set_text("Plan the launch");
        assert!(surface.send_composer());
        while commands.try_recv().is_ok() {}
        assert!(surface.composer.is_blank());
        let failure = koru_ai_wire::Failure {
            code: "ai_storage".into(),
            message: "The chat could not be saved.".into(),
            retryable: true,
            details: Default::default(),
        };
        surface.apply_update(Update::CreateFailed(chat, failure.clone()));
        assert!(surface.chat_unmade(), "a stand-in again");
        let reason = surface.status.clone();
        assert!(reason.is_some());
        surface.apply_update(Update::Failure("Chat not found.".into()));
        surface.apply_update(Update::SendFailed(failure));
        assert_eq!(surface.composer.text(), "Plan the launch", "it is back");
        assert!(surface.pending_user.is_none());
        assert_eq!(surface.status, reason, "with the create's reason");

        assert!(surface.send_composer());
        assert!(
            matches!(
                commands.recv_timeout(Duration::from_millis(50)).unwrap(),
                Command::Create(request) if request.chat_id == chat
            ),
            "the next send asks for the chat again"
        );
    }

    /// An existing host folder choice stays with a not-yet-created chat even
    /// though the composer no longer offers a folder chooser.
    #[test]
    fn a_new_chats_folder_is_kept_until_its_first_message() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        while commands.try_recv().is_ok() {}
        let chat = surface.transcript.as_ref().unwrap().chat.id;
        assert!(!surface.shows_composer_footer(), "no folder row is shown");
        assert!(surface.working_folder_chosen(chat, "D:/work/koru".into()));
        assert!(commands.try_recv().is_err(), "kept here, not sent");
        assert_eq!(surface.folder_label(), "koru");
        surface.composer.set_text("Plan the launch");
        assert!(surface.send_composer());
        let order: Vec<&'static str> = std::iter::from_fn(|| commands.try_recv().ok())
            .map(|command| match command {
                Command::Create(_) => "create",
                Command::UpdateFolder(about, request) => {
                    assert_eq!(about, chat);
                    assert_eq!(request.folder.as_deref(), Some("D:/work/koru"));
                    "folder"
                }
                Command::Send(_) => "send",
                Command::Rename(..) => "rename",
                _ => "other",
            })
            .collect();
        assert_eq!(order, ["create", "folder", "send", "rename"]);
    }

    /// **A chat not made yet stays on screen** when the list changes under
    /// it: deleting another chat does not swap in the service's current one.
    #[test]
    fn a_chat_not_made_yet_survives_a_history_change() {
        let (mut surface, _commands, _updates) = unmade_claude_chat();
        let chat = surface.transcript.as_ref().unwrap().chat.id;
        let other = transcript();
        surface.apply_update(Update::HistoryChanged {
            chats: vec![other.chat.clone()],
            transcript: Some(other),
        });
        assert_eq!(surface.transcript.as_ref().map(|t| t.chat.id), Some(chat));
        assert!(surface.chat_unmade());
    }

    /// **A picture pasted into a chat not made yet makes it** — a picture is
    /// stored under its chat — and the paste is read once the chat exists.
    #[test]
    fn a_paste_into_a_chat_not_made_yet_makes_it_first() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        while commands.try_recv().is_ok() {}
        let chat = surface.transcript.as_ref().unwrap().chat.id;
        assert!(surface.begin_attachment_paste());
        assert!(
            !surface.clipboard_reading,
            "not read before the chat exists"
        );
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Create(request) if request.chat_id == chat
        ));
        assert_eq!(surface.paste_after_create, Some(chat));
    }

    /// **A chat chosen before opens as it did, without being made**: the
    /// remembered provider validates and the chat view shows a new chat on
    /// it; the first message makes it — once, not twice (a tab that owed a
    /// fresh chat asked for two).
    #[test]
    fn a_remembered_provider_makes_one_chat_for_a_fresh_tab() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.start_new_chat());
        let mut settings = unchosen_settings(&[]);
        settings.selected_provider = Some(AiProviderId::parse(AiProviderId::KORU).unwrap());
        boot(&mut surface, settings);
        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::KORU).unwrap(),
            status: ProviderValidationStatus::Ready,
            models: vec![AiProviderId::KORU_HOSTED_MODEL.into()],
            selected_model: None,
            failure: None,
        }));
        assert!(
            !std::iter::from_fn(|| commands.try_recv().ok())
                .any(|command| matches!(command, Command::Create(_))),
            "nothing is made before a message"
        );
        assert!(surface.chat_unmade());
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .map(|t| t.chat.current_model.provider.as_str()),
            Some(AiProviderId::KORU)
        );
        surface.composer.set_text("hello");
        assert!(surface.send_composer());
        let creates = std::iter::from_fn(|| commands.try_recv().ok())
            .filter(|command| matches!(command, Command::Create(_)))
            .count();
        assert_eq!(creates, 1);
        assert!(!surface.showing_setup());
    }

    /// **A chat chosen before whose provider is down** opens on the chat view
    /// too, the provider's one line above the composer.
    #[test]
    fn a_remembered_provider_that_is_down_says_so_on_the_chat_view() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut settings = unchosen_settings(&[]);
        settings.selected_provider = Some(AiProviderId::parse(AiProviderId::KORU).unwrap());
        boot(&mut surface, settings);
        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::KORU).unwrap(),
            status: ProviderValidationStatus::Unavailable,
            models: Vec::new(),
            selected_model: None,
            failure: None,
        }));
        assert!(!surface.showing_setup());
        assert_eq!(
            surface.status.as_deref(),
            Some(koru_i18n::t(
                surface.lang,
                koru_i18n::Key::AiKoruUnreachable
            ))
        );
    }

    /// **The page's *Open in chat* seeds the composer with its chip**, the
    /// way `@` puts one there; anything typed stays after it.
    #[test]
    fn attaching_a_document_puts_its_chip_in_the_composer() {
        let mut surface = ChatSurface::new();
        let document = DocumentId::new();
        surface.composer.set_text("summarise");
        assert!(surface.attach_document(document, "Launch plan".into()));
        let message = surface.composer.message();
        assert_eq!(message.documents, vec![(document, "Launch plan".into())]);
        assert!(message.markdown.ends_with("summarise"), "{message:?}");
    }

    /// **A chat's first message names it** (the review's note), the way ChatGPT
    /// names a chat: the first line, cut at a word, Markdown marks stripped.
    /// A chat already named keeps its name.
    #[test]
    fn the_first_message_names_a_new_chat() {
        assert_eq!(
            title_from_message("# Plan the launch\n\nmore"),
            Some("Plan the launch".into())
        );
        assert_eq!(
            title_from_message(
                "Write a release note for the workspace foundation that covers the rail and the floor"
            ),
            Some("Write a release note for the workspace…".into())
        );
        assert_eq!(title_from_message("   \n\n"), None);
        assert_eq!(
            title_from_message("> quoted question?"),
            Some("quoted question?".into())
        );

        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.title = DEFAULT_CHAT_TITLE.into();
        let id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        assert!(surface.key(
            &Key::Character("h".into()),
            Some("How do I pin a tab"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(_) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("expected a send command");
        };
        let Command::Rename(renamed, request) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("expected the rename after the send");
        };
        assert_eq!(renamed, id);
        assert_eq!(request.title, "How do I pin a tab");

        // Named already: no rename rides the next send.
        let mut chat = transcript();
        chat.chat.title = "Pins".into();
        surface.transcript = Some(chat);
        assert!(surface.key(
            &Key::Character("a".into()),
            Some("and again"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(_) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("expected a send command");
        };
        assert!(commands.try_recv().is_err(), "no rename for a named chat");
    }

    /// A first message that is a document chip — *Open in chat* — names the
    /// chat after the document's title, never its `koru://` link.
    #[test]
    fn a_document_chip_names_the_chat_by_its_title() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.title = DEFAULT_CHAT_TITLE.into();
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        assert!(surface.attach_document(DocumentId::new(), "Quarterly budget".into()));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        let Command::Send(_) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
            panic!("expected a send command");
        };
        let Command::Rename(_, request) = commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("expected the rename after the send");
        };
        assert_eq!(request.title, "Quarterly budget");
    }

    /// **The wheel scrolls the picker before a chat exists**: its rows are
    /// the selected agent's models, not the chat's own, of which there are
    /// none yet.
    #[test]
    fn the_wheel_scrolls_a_long_picker_with_no_chat() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        surface.settings = Some(unchosen_settings(&["agent-claude"]));
        let claude = AiProviderId::parse("agent-claude").unwrap();
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::Ready);
        let models = (0..model_picker::LIST_ROWS + 6)
            .map(|n| koru_ai_wire::AgentModelView {
                id: format!("model-{n}"),
                group: "claude".into(),
                name: None,
                description: None,
                group_name: None,
                legacy: false,
                reasoning: None,
                context: None,
                fast: None,
            })
            .collect();
        surface
            .agent_models
            .insert(claude, agents::AgentModels::Listed(models));
        surface.composer_rect();
        assert!(surface.open_model_picker());
        assert!(surface.picker_rows().len() > model_picker::LIST_ROWS);
        assert!(surface.scroll_by(MouseScrollDelta::LineDelta(0.0, -1.0)));
        assert_eq!(surface.picker.offset, 1);
        assert!(surface.scroll_by(MouseScrollDelta::LineDelta(0.0, 1.0)));
        assert_eq!(surface.picker.offset, 0);
    }

    #[test]
    fn web_search_is_capability_gated_sticky_and_streams_visible_sources() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        chat.chat.current_model.requested_model = "gpt-5.4".into();
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();

        assert!(surface.web_search_available());
        assert!(surface.web_search_armed, "web search starts on");
        surface.open_plus_menu();
        let search = surface
            .plus_menu
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .position(|item| matches!(item, composer_plus::PlusItem::SearchWeb(_, true)))
            .expect("the available OpenAI chat offers web search in +");
        assert!(surface.choose_plus_item(search));
        assert!(!surface.web_search_armed);
        assert!(surface.choose_plus_item(search));
        assert!(surface.web_search_armed);
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(),));
        assert!(surface.key(
            &Key::Character("q".into()),
            Some("question"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        let Command::Send(request) = commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("expected a send command");
        };
        assert!(request.web_search);

        let attempt_id = InferenceAttemptId::new();
        let update = |event| {
            Update::Event(koru_ai_wire::StreamEvent {
                version: koru_ai_wire::AI_STREAM_VERSION,
                sequence: 1,
                event,
            })
        };
        surface.apply_update(update(PublicAiEvent::WebSearch {
            attempt_id,
            call_id: "search-1".into(),
            phase: WebSearchPhase::Started,
            at: koru_ai_core::Timestamp::from_millis(2),
        }));
        let width = surface.conversation_rect().w;
        surface.rebuild_messages(width);
        assert_eq!(
            surface.message_layouts.last().unwrap().display,
            "Searching the web…"
        );

        surface.apply_update(update(PublicAiEvent::Citation {
            attempt_id,
            source: SourceLink {
                url: "https://example.com/source".into(),
                title: "Example source".into(),
            },
            at: koru_ai_core::Timestamp::from_millis(3),
        }));
        surface.rebuild_messages(width);
        let pending = surface.message_layouts.last().unwrap();
        assert_eq!(pending.sources.as_ref().unwrap().sources().count(), 1);

        surface.apply_update(update(PublicAiEvent::TextDelta {
            attempt_id,
            text: "Answer".into(),
            at: koru_ai_core::Timestamp::from_millis(4),
        }));
        surface.rebuild_messages(width);
        assert_eq!(surface.message_layouts.last().unwrap().display, "Answer");

        surface.transcript.as_mut().unwrap().chat.current_model = ModelSelection {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            requested_model: "qwen3:4b".into(),
            default_reasoning: ReasoningChoice::Default,
        };
        assert!(!surface.web_search_available());
        surface.open_plus_menu();
        assert!(
            !surface
                .plus_menu
                .as_ref()
                .unwrap()
                .items()
                .iter()
                .any(|item| matches!(item, composer_plus::PlusItem::SearchWeb(..)))
        );
    }

    /// **A reply's picture from a file is asked of the disk loader** (the
    /// user's call, 2026-11-03): a `file:` URL is read through D38's loader
    /// under its path, in the reply's picture box; a relative path is never
    /// read, and keeps the alt text as before — as does a file that is not
    /// there (under test nothing is read, so it reads as not there).
    #[test]
    fn a_replys_file_picture_is_read_from_the_disk() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let url = if cfg!(windows) {
            "file:///D:/apple.jpeg"
        } else {
            "file:///home/me/apple.jpeg"
        };
        let mut ordinal = 0;
        for markdown in [
            format!("![Apple logo]({url})"),
            "![Pear](pictures/pear.png)".to_owned(),
        ] {
            let message_id = MessageId::new();
            transcript.messages.push(koru_ai_core::Message {
                id: message_id,
                chat_id: transcript.chat.id,
                ordinal,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                markdown: markdown.clone(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
            });
            transcript.blocks.push(koru_ai_core::MessageBlock {
                id: MessageBlockId::new(),
                message_id,
                ordinal: 0,
                kind: koru_ai_core::MessageBlockKind::Paragraph,
                plain_text: markdown.clone(),
                markdown,
            });
            ordinal += 1;
        }
        surface.transcript = Some(transcript);
        surface.rebuild_messages(600.0);
        let images: Vec<&ImageSpec> = surface
            .message_layouts
            .iter()
            .flat_map(|message| message.blocks.iter())
            .filter_map(|block| block.image.as_ref())
            .collect();
        assert_eq!(images.len(), 2, "both keep their picture box");
        assert_eq!(images[0].alt, "Apple logo");
        let key = image_key(url);
        assert_ne!(key, url, "kept under its path");
        assert_eq!(
            Some(std::path::PathBuf::from(&key)),
            tool_output::picture::reply_picture_path(url)
        );
        assert!(surface.images.get(&key).is_some_and(|image| image.failed));
        assert_eq!(surface.images.len(), 1, "a relative path is never read");
        assert_eq!(image_key("pictures/pear.png"), "pictures/pear.png");
    }

    /// **A font change lays every message out again** (found with D79): a
    /// finished message is otherwise reused as it was laid out — its text,
    /// width and blocks unchanged — so it kept the old font after the App
    /// font changed, `invalidate` keeping `message_layouts`.
    #[test]
    fn invalidate_lays_every_finished_message_out_again() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let message_id = MessageId::new();
        transcript.messages.push(koru_ai_core::Message {
            id: message_id,
            chat_id: transcript.chat.id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: "A durable answer".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        });
        transcript.blocks.push(koru_ai_core::MessageBlock {
            id: MessageBlockId::new(),
            message_id,
            ordinal: 0,
            kind: koru_ai_core::MessageBlockKind::Paragraph,
            markdown: "A durable answer".into(),
            plain_text: "A durable answer".into(),
        });
        surface.transcript = Some(transcript);
        surface.rebuild_messages(600.0);
        surface.rebuild_messages(600.0);
        assert_eq!(
            surface.last_rebuild_stats.messages_reused, 1,
            "unchanged, it is reused"
        );
        surface.invalidate();
        assert!(surface.messages_dirty);
        surface.rebuild_messages(600.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 0);
        assert_eq!(surface.last_rebuild_stats.blocks_reused, 0);
        assert_eq!(surface.last_rebuild_stats.messages_laid_out, 1);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 1);
    }

    /// The chat font is a process global; the tests that set it take this.
    static CHAT_FONT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const WORDS: &str = "The quick brown fox jumps over the lazy dog";

    /// A transcript with one finished question and one finished answer, both
    /// [`WORDS`].
    fn question_and_answer() -> ChatTranscript {
        let mut transcript = transcript();
        for (ordinal, role) in [ChatRole::User, ChatRole::Assistant]
            .into_iter()
            .enumerate()
        {
            let message_id = MessageId::new();
            transcript.messages.push(koru_ai_core::Message {
                id: message_id,
                chat_id: transcript.chat.id,
                ordinal: ordinal as u32,
                role,
                state: MessageState::Complete,
                markdown: WORDS.into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
            });
            if role == ChatRole::Assistant {
                transcript.blocks.push(koru_ai_core::MessageBlock {
                    id: MessageBlockId::new(),
                    message_id,
                    ordinal: 0,
                    kind: koru_ai_core::MessageBlockKind::Paragraph,
                    markdown: WORDS.into(),
                    plain_text: WORDS.into(),
                });
            }
        }
        transcript
    }

    /// The width [`WORDS`] takes on one line in `face`, at the chat's size
    /// and the weight `weight`.
    fn words_width(face: theme::FontChoice, weight: theme::FontWeight) -> f32 {
        TextEngine::new()
            .simple_layout_wrapped_in(
                WORDS,
                role::ui().with_weight(weight),
                face.stack(),
                f32::MAX,
            )
            .width()
    }

    /// The laid-out widths of the question's text and the answer's block.
    fn drawn_widths(surface: &ChatSurface) -> (f32, f32) {
        let question = surface
            .message_layouts
            .iter()
            .find(|message| message.role == ChatRole::User)
            .expect("the question");
        let answer = surface
            .message_layouts
            .iter()
            .find(|message| message.role == ChatRole::Assistant)
            .expect("the answer");
        (
            question.layout.width(),
            answer.blocks.first().expect("its block").layout.width(),
        )
    }

    /// **The chat draws in the Chat font** (koru-ai D79): a person's
    /// message and the agent's answer are both set in it, not in the app
    /// font, which stays as it was.
    #[test]
    fn the_chat_draws_in_the_chat_font() {
        let _serial = CHAT_FONT.lock().unwrap_or_else(|held| held.into_inner());
        let was = theme::font_setting(theme::FontSlot::Chat);
        theme::set_chat_font(theme::FontChoice::JetBrainsMono);
        let mono = theme::FontChoice::JetBrainsMono;
        assert_ne!(theme::ui_font(), mono, "a face the app is not set in");

        let mut surface = ChatSurface::new();
        surface.transcript = Some(question_and_answer());
        surface.rebuild_messages(1200.0);
        let (question, answer) = drawn_widths(&surface);
        theme::set_font_setting(theme::FontSlot::Chat, was);

        assert_eq!(question, words_width(mono, theme::FontWeight::Medium));
        assert_ne!(
            question,
            words_width(theme::ui_font(), theme::FontWeight::Medium),
            "not the app font"
        );
        let answer_weight = mono.weight_for(None);
        let in_mono = TextEngine::new()
            .simple_layout_at_weight(WORDS, role::ui().metrics, mono.stack(), answer_weight)
            .width();
        assert_eq!(answer, in_mono);
    }

    /// **A Chat font change lays the finished messages out again** (D82's
    /// path, which an App font change takes): reused while nothing changes,
    /// laid out afresh after the host's `invalidate`, and drawn in the new
    /// face.
    #[test]
    fn a_chat_font_change_lays_finished_messages_out_again() {
        let _serial = CHAT_FONT.lock().unwrap_or_else(|held| held.into_inner());
        let was = theme::font_setting(theme::FontSlot::Chat);
        let mono = theme::FontChoice::JetBrainsMono;
        theme::set_chat_font(theme::FontChoice::Lora);

        let mut surface = ChatSurface::new();
        surface.transcript = Some(question_and_answer());
        surface.rebuild_messages(1200.0);
        surface.rebuild_messages(1200.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 2);
        let in_lora = drawn_widths(&surface).0;

        theme::set_chat_font(mono);
        surface.invalidate();
        surface.rebuild_messages(1200.0);
        let stats = surface.last_rebuild_stats;
        let (question, _) = drawn_widths(&surface);
        theme::set_font_setting(theme::FontSlot::Chat, was);

        assert_eq!(stats.messages_reused, 0);
        assert_eq!(stats.blocks_reused, 0);
        assert_eq!(stats.messages_laid_out, 2);
        assert_eq!(stats.blocks_laid_out, 1);
        assert_eq!(question, words_width(mono, theme::FontWeight::Medium));
        assert_ne!(question, in_lora, "the new face, not the kept layout");
    }

    #[test]
    fn a_stream_event_relays_out_only_the_pending_turn() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let message_id = MessageId::new();
        transcript.messages.push(koru_ai_core::Message {
            id: message_id,
            chat_id: transcript.chat.id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: "A durable answer".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        });
        transcript.blocks.push(koru_ai_core::MessageBlock {
            id: MessageBlockId::new(),
            message_id,
            ordinal: 0,
            kind: koru_ai_core::MessageBlockKind::Paragraph,
            markdown: "A **durable** answer".into(),
            plain_text: "A durable answer".into(),
        });
        surface.transcript = Some(transcript);
        surface.pending_user = Some("What next?".into());
        surface.draft = "First".into();
        surface.rebuild_messages(400.0);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 1);

        surface.draft.push_str(" delta");
        surface.rebuild_messages(400.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 1);
        assert_eq!(surface.last_rebuild_stats.blocks_reused, 1);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 0);

        surface.rebuild_messages(500.0);
        assert_eq!(surface.last_rebuild_stats.messages_reused, 0);
        assert_eq!(surface.last_rebuild_stats.blocks_laid_out, 1);
    }

    #[test]
    fn an_invalid_table_renders_as_its_source_text() {
        let source = "| A | B |\n| --- |";
        let mut engine = TextEngine::new();
        let block = render_chat_block(&mut engine, MessageBlockId::new(), source, 300.0);
        assert!(block.table.is_none());
        assert_eq!(block.text, source);
    }

    #[test]
    fn missing_authoritative_attempt_ends_recovery_with_retryable_status() {
        let mut surface = ChatSurface::new();
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        surface.waiting = true;
        surface.pending_assistant_message_id = Some(MessageId::new());
        surface.pending_user = Some("Question that never became authoritative".into());
        surface.draft = "Partial answer".into();
        surface.pending_question_anchor = Some(42.0);
        surface.scroll_to_pending_question = true;
        surface.messages_dirty = false;

        assert!(surface.apply_update(Update::RecoveryFailed {
            attempt,
            message: "This response could not be recovered. You can retry it.".into(),
        }));

        assert_eq!(surface.attempt, None);
        assert!(!surface.waiting);
        assert_eq!(surface.pending_assistant_message_id, None);
        assert_eq!(surface.pending_user, None);
        assert!(surface.draft.is_empty());
        assert_eq!(surface.pending_question_anchor, None);
        assert!(!surface.scroll_to_pending_question);
        assert!(surface.messages_dirty);
        assert_eq!(
            surface.status.as_deref(),
            Some("This response could not be recovered. You can retry it.")
        );
    }

    #[test]
    fn stale_recovery_failure_does_not_clear_a_newer_attempt() {
        let mut surface = ChatSurface::new();
        let stale = InferenceAttemptId::new();
        let current = InferenceAttemptId::new();
        surface.attempt = Some(current);
        surface.waiting = true;

        assert!(surface.apply_update(Update::RecoveryFailed {
            attempt: stale,
            message: "stale".into(),
        }));

        assert_eq!(surface.attempt, Some(current));
        assert!(surface.waiting);
        assert_ne!(surface.status.as_deref(), Some("stale"));
    }

    /// A side command that fails while a reply streams — a rename, say —
    /// leaves the reply running: Stop stays, and so does the reply's id.
    /// The reply then ends as any other does. A failure after its stream
    /// has ended — the transcript read that follows it — does not keep
    /// Stop.
    #[test]
    fn a_side_command_failing_mid_reply_keeps_stop() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let transcript = transcript();
        let chat = transcript.chat.id;
        let event = |event| {
            Update::Event(koru_ai_wire::StreamEvent {
                version: koru_ai_wire::AI_STREAM_VERSION,
                sequence: 1,
                event,
            })
        };
        let at = koru_ai_core::Timestamp::from_millis(2);
        let accept = |surface: &mut ChatSurface| {
            surface.transcript = Some(transcript.clone());
            surface.pending_user = Some("Plan the launch".into());
            let attempt_id = InferenceAttemptId::new();
            let reply = MessageId::new();
            surface.apply_update(Update::Accepted(koru_ai_wire::SendTurnAccepted {
                attempt_id,
                user_message_id: MessageId::new(),
                assistant_message_id: reply,
                replayed: false,
            }));
            surface.apply_update(event(PublicAiEvent::TextDelta {
                attempt_id,
                text: "First, the date.".into(),
                at,
            }));
            (attempt_id, reply)
        };

        let (attempt_id, reply) = accept(&mut surface);
        surface.apply_update(Update::Failure("The chat could not be renamed.".into()));
        assert_eq!(surface.attempt, Some(attempt_id), "Stop stays");
        assert_eq!(surface.pending_assistant_message_id, Some(reply));
        assert!(surface.waiting, "the stream is still read");
        assert_eq!(
            surface.status.as_deref(),
            Some("The chat could not be renamed.")
        );
        assert!(commands.try_recv().is_err());

        // The reply ends normally: its terminal event reads the chat, and
        // the transcript that answers clears the turn.
        surface.apply_update(event(PublicAiEvent::Completed {
            attempt_id,
            stop_reason: None,
            at,
        }));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Read(read) if read == chat
        ));
        surface.apply_update(Update::Transcript(transcript.clone()));
        assert_eq!(surface.attempt, None);
        assert_eq!(surface.pending_assistant_message_id, None);

        // A reply whose stream has ended, and whose transcript read fails.
        let (attempt_id, _) = accept(&mut surface);
        surface.apply_update(event(PublicAiEvent::Completed {
            attempt_id,
            stop_reason: None,
            at,
        }));
        while commands.try_recv().is_ok() {}
        surface.apply_update(Update::Failure("Chat not found.".into()));
        assert_eq!(surface.attempt, None, "no Stop for a reply that has ended");
        assert!(!surface.waiting);
    }

    #[test]
    fn composer_grows_to_eight_lines_then_caps() {
        let mut surface = ChatSurface::new();
        let base = surface.composer_rect().h;
        for _ in 0..12 {
            surface.composer.insert_break();
        }
        let grown = surface.composer_rect().h;
        // A composer line is a line of the app's text; a break is a new
        // paragraph, with the editor's small gap between paragraphs.
        let line = chat_line_height();
        let gaps = theme::TASK_GAP * (COMPOSER_MAX_LINES - 1) as f32;
        assert!(grown > base);
        assert!(
            grown
                <= line * COMPOSER_MAX_LINES as f32
                    + gaps
                    + COMPOSER_PAD_TOP
                    + COMPOSER_TEXT_GAP
                    + COMPOSER_TOOLBAR_H
                    + COMPOSER_PAD_BOTTOM
                    + 1.0
        );
        // Four lines are four lines: the composer grows until the eighth.
        let mut short = ChatSurface::new();
        short.composer.set_text("1\n2\n3\n4");
        let four = short.composer_rect().h;
        assert!(four > base && four < grown);
    }

    #[test]
    fn alt_arrows_visit_questions_then_return_to_the_composer() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for (ordinal, role) in [
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::User,
            ChatRole::Assistant,
        ]
        .into_iter()
        .enumerate()
        {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal: ordinal as u32,
                role,
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.messages_dirty = true;
        let alt = ModifiersState::ALT;

        assert!(surface.key(&Key::Named(NamedKey::ArrowUp), None, alt));
        assert_eq!(surface.question_navigation, Some(2));
        assert_eq!(surface.focus, Focus::None);
        assert!(surface.key(&Key::Named(NamedKey::ArrowUp), None, alt));
        assert_eq!(surface.question_navigation, Some(1));
        assert!(surface.key(&Key::Named(NamedKey::ArrowDown), None, alt));
        assert_eq!(surface.question_navigation, Some(2));
        assert!(surface.key(&Key::Named(NamedKey::ArrowDown), None, alt));
        assert_eq!(surface.question_navigation, None);
        assert_eq!(surface.focus, Focus::Composer);
    }

    #[test]
    fn loaded_chat_scrolls_to_its_end_once() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..24 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.messages_dirty = true;
        surface.scroll_loaded_chat_to_bottom = true;

        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        assert!(surface.conversation_scroll > 0.0);
        let conversation_height = surface.conversation_rect().h;
        let expected_scroll = (surface.conversation_extent - conversation_height).max(0.0);
        assert_eq!(surface.conversation_scroll, expected_scroll,);
        assert!(!surface.scroll_loaded_chat_to_bottom);
    }

    /// **↓ and Ctrl+End jump to the newest message** (D77): the button shows
    /// only while the transcript is scrolled up from its end, centred above
    /// the composer; a press or Ctrl+End scrolls to the end and follows a
    /// reply that keeps growing, until the person scrolls up again. In a
    /// composer with text Ctrl+End stays the editor's.
    #[test]
    fn jump_to_latest_shows_off_the_end_and_follows_the_reply() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        let message = |chat, ordinal: u32, markdown: String| koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: chat,
            ordinal,
            role: if ordinal % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            state: MessageState::Complete,
            markdown,
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(i64::from(ordinal) + 1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(i64::from(ordinal) + 1)),
        };
        let chat = transcript.chat.id;
        for ordinal in 0..24 {
            transcript
                .messages
                .push(message(chat, ordinal, format!("message {ordinal}")));
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.messages_dirty = true;
        surface.scroll_loaded_chat_to_bottom = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        let max = |surface: &ChatSurface| {
            (surface.conversation_extent - surface.conversation_hit_rect.h).max(0.0)
        };
        assert!(surface.jump_button().is_none(), "at the end: no button");

        // Scrolled up: the button, centred over the transcript, above the
        // composer.
        surface.scroll_by(MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(surface.conversation_scroll < max(&surface));
        let button = surface.jump_button().expect("scrolled up: the button");
        let area = surface.conversation_hit_rect;
        assert_eq!((button.w, button.h), (32.0, 32.0));
        assert!((button.x + button.w * 0.5 - (area.x + area.w * 0.5)).abs() < 0.01);
        assert!(button.y + button.h <= area.y + area.h);
        surface.pointer = (button.x + 16.0, button.y + 16.0);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default, "the arrow");

        assert!(surface.mouse_down(button.x + 16.0, button.y + 16.0));
        assert_eq!(surface.conversation_scroll, max(&surface));
        assert!(surface.jump_button().is_none());

        // A reply that keeps growing is followed.
        let grow = |surface: &mut ChatSurface, ordinal: u32| {
            let transcript = surface.transcript.as_mut().unwrap();
            transcript.messages.push(message(
                chat,
                ordinal,
                "A longer reply\n\nwith more\n\nparagraphs".into(),
            ));
            surface.messages_dirty = true;
            surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        };
        let before = surface.conversation_scroll;
        grow(&mut surface, 25);
        assert!(surface.conversation_scroll > before, "followed");
        assert_eq!(surface.conversation_scroll, max(&surface));

        // Scrolling up lets go: the next growth stays where the reader is.
        surface.scroll_by(MouseScrollDelta::LineDelta(0.0, 2.0));
        let reading = surface.conversation_scroll;
        grow(&mut surface, 27);
        assert_eq!(surface.conversation_scroll, reading, "not pulled down");

        // Ctrl+End with an empty composer jumps; with text it is the editor's.
        let end = Key::Named(NamedKey::End);
        let primary = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        surface.composer.set_text("draft");
        surface.key(&end, None, primary);
        assert_eq!(
            surface.conversation_scroll, reading,
            "the caret's, not ours"
        );
        surface.composer.set_text("");
        assert!(surface.key(&end, None, primary));
        assert_eq!(surface.conversation_scroll, max(&surface));
        assert_eq!(
            koru_keys::registry::get(koru_keys::ShortcutId::JumpToLatest).layer,
            koru_keys::Layer::Chat
        );
    }

    /// **↓ waits until the end is well out of sight** ([`JUMP_THRESHOLD`];
    /// the user: *only show the go to bottom when the last item in the chat
    /// crosses the composer by 20% viewport size*): the last item's bottom a
    /// few pixels under the composer's top, or just short of a fifth of the
    /// conversation viewport under it, shows no button; just past it, the
    /// button. `Ctrl+End` jumps either way.
    #[test]
    fn jump_to_latest_waits_for_a_fifth_of_the_viewport_under_the_composer() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        let chat = transcript.chat.id;
        for ordinal in 0..24_u32 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: chat,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(i64::from(ordinal) + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(i64::from(ordinal) + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 720.0);
        surface.messages_dirty = true;
        surface.scroll_loaded_chat_to_bottom = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        let area = surface.conversation_hit_rect;
        let composer_top = surface.composer_hit_rect.y;
        assert!(composer_top >= area.y + area.h, "the composer is below");
        let extent = surface.conversation_content_extent();
        // The scroll at which the last item's bottom is `under` below the
        // composer's top.
        let scroll_for = |under: f32| area.y + extent - composer_top - under;
        let threshold = area.h * JUMP_THRESHOLD;
        let max = (surface.conversation_extent - area.h).max(0.0);
        assert!(scroll_for(threshold + 1.0) > 0.0 && scroll_for(4.0) < max);

        // The screenshot: the last line just behind where ↓ would be.
        surface.conversation_scroll = scroll_for(4.0);
        assert!(surface.jump_button().is_none(), "a few pixels under");
        surface.conversation_scroll = scroll_for(threshold - 1.0);
        assert!(surface.jump_button().is_none(), "just under the threshold");
        surface.conversation_scroll = scroll_for(threshold + 1.0);
        assert!(surface.jump_button().is_some(), "just over it");

        // Under the threshold, Ctrl+End still jumps.
        surface.conversation_scroll = scroll_for(threshold - 1.0);
        let primary = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        assert!(surface.key(&Key::Named(NamedKey::End), None, primary));
        assert_eq!(surface.conversation_scroll, max);
    }

    #[test]
    fn authoritative_recovery_preserves_same_chat_scroll_position() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..24 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        let max_scroll = (surface.conversation_extent - surface.conversation_rect().h).max(0.0);
        surface.conversation_scroll = max_scroll * 0.4;
        let before = surface.conversation_scroll;
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        surface.waiting = true;
        let authoritative = surface.transcript.clone().unwrap();

        surface.apply_update(Update::Recovered {
            attempt,
            status: koru_ai_core::AttemptStatus::Complete,
            transcript: authoritative,
        });
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        assert!((surface.conversation_scroll - before).abs() < 0.01);
        assert!(!surface.waiting);
    }

    #[test]
    fn completed_short_answer_keeps_the_submitted_question_at_the_top() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.tool_policy.chat_id = transcript.chat.id;
        for ordinal in 0..10 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("older message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.pending_user = Some("Another one".into());
        surface.scroll_to_pending_question = true;
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        let anchored = surface.conversation_scroll;
        assert_eq!(surface.pending_question_anchor, Some(anchored));

        let transcript = surface.transcript.as_mut().unwrap();
        for (ordinal, role, markdown) in [
            (10, ChatRole::User, "Another one"),
            (11, ChatRole::Assistant, "A short answer"),
        ] {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role,
                state: MessageState::Complete,
                markdown: markdown.into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        surface.pending_user = None;
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        let natural_max =
            (surface.conversation_content_extent() - surface.conversation_rect().h).max(0.0);
        assert!(natural_max < anchored);
        assert!((surface.conversation_scroll - anchored).abs() < 0.01);
    }

    #[test]
    fn workspace_selection_replaces_the_complete_chat_grant_set() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat = transcript.chat.id;
        let workspace = WorkspaceId::new();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);

        assert!(surface.toggle_tool_workspace(workspace));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateToolPolicy(actual, request) => {
                assert_eq!(actual, chat);
                assert_eq!(request.mode, ToolMode::Manual);
                assert_eq!(request.workspace_ids, vec![workspace]);
            }
            _ => panic!("workspace selection must replace the chat policy"),
        }
    }

    #[test]
    fn embedded_context_workspace_is_prioritized_and_granted_before_send() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let current = WorkspaceId::new();
        let other = WorkspaceId::new();
        let mut transcript = transcript();
        let chat = transcript.chat.id;
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.tool_policy.workspace_ids = vec![other];
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.destinations = vec![
            destination(other, "Other / Workspace"),
            destination(current, "Current / Workspace"),
        ];

        assert!(surface.set_context_workspace(Some(current)));
        assert_eq!(surface.destinations[0].workspace_id, current);
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .unwrap()
                .tool_policy
                .workspace_ids,
            vec![other]
        );
        assert!(
            commands.try_recv().is_err(),
            "selection cannot replace another window's grants"
        );
        surface.composer.set_text("Send from here");
        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::AddGrants(actual, selected, documents, _) => {
                assert_eq!(actual, chat);
                assert_eq!(selected, Some(current));
                assert!(documents.is_empty());
            }
            _ => panic!("send must request an atomic union"),
        }
        assert!(!surface.set_context_workspace(Some(current)));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn composer_footer_keeps_search_in_plus_and_separates_approval_and_model() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        transcript.chat.current_model.requested_model = "gpt-5.5".into();
        surface.transcript = Some(transcript);

        let toolbar = surface.composer_toolbar_rect();
        let plus = surface.plus_button();
        surface.lang = koru_i18n::Lang::En;
        let approval = surface
            .tool_mode_button()
            .expect("a Koru-run chat has the mode chip");
        let model = surface.chat_model_button();
        // The private mark is hidden for now; search is in the plus menu.
        assert_eq!(surface.privacy_button(), None);
        assert!(surface.web_search_button().is_none());
        surface.open_plus_menu();
        assert!(
            surface
                .plus_menu
                .as_ref()
                .unwrap()
                .items()
                .iter()
                .any(|item| matches!(item, composer_plus::PlusItem::SearchWeb(_, true)))
        );

        assert_eq!(plus.x, toolbar.x);
        assert!((model.x + model.w - toolbar.x - toolbar.w).abs() < 0.01);
        assert!(plus.x + plus.w < approval.x);
        assert!(approval.x + approval.w < model.x);
        assert_eq!(surface.tool_access_label(), "0 spaces");
        assert_eq!(surface.tool_mode_label(), "Manual");
    }

    #[test]
    fn workspace_access_adds_search_only_for_large_lists_and_filters_rows() {
        let mut surface = ChatSurface::new();
        surface.transcript = Some(transcript());
        let profile = ProfileId::new();
        surface.destinations = (0..6)
            .map(|index| DestinationOption {
                profile_id: profile,
                ..destination(WorkspaceId::new(), &format!("Workspace {index}"))
            })
            .collect();
        surface.context_workspace = Some(surface.destinations[0].workspace_id);
        surface.tool_access_open = true;

        assert!(surface.tool_access_search_rect().is_some());
        surface
            .workspace_search
            .set_text(&mut surface.engine, "Workspace 4");
        let matches = surface.matching_workspace_indices();
        assert_eq!(matches, vec![4]);

        surface.destinations.truncate(5);
        assert!(surface.tool_access_search_rect().is_none());
    }

    #[test]
    fn chat_model_picker_is_wide_and_scrollable() {
        let mut surface = ChatSurface::new();
        // A chat that has begun: an empty one offers only the agents.
        let mut begun = transcript();
        begun.messages.push(koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: begun.chat.id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "hi".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
        });
        surface.transcript = Some(begun);
        surface.openai_models = (0..12).map(|index| format!("gpt-5.{index}")).collect();
        surface.composer_rect();
        assert!(surface.open_model_picker());

        let geometry = surface.picker_geometry().expect("model panel");
        let button = surface.chat_model_button();
        assert!(geometry.panel.w > button.w);
        assert!(surface.picker_scrollbar(&geometry).is_some());
    }

    #[test]
    fn composer_owns_its_disabled_send_action() {
        let mut surface = ChatSurface::new();
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let composer = surface.composer_rect();
        let send = surface.send_button();
        assert!(composer.contains(send.x + send.w * 0.5, send.y + send.h * 0.5));
        assert!(!surface.send_composer());
        assert!(surface.mouse_down(send.x + send.w * 0.5, send.y + send.h * 0.5));
    }

    #[test]
    fn shift_enter_inserts_a_newline_in_the_composer() {
        let mut surface = ChatSurface::new();
        surface.validated_ready = true;
        surface.composer.set_text("one");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT,));
        assert_eq!(surface.composer.text(), "one\n");
    }

    #[test]
    fn ctrl_j_inserts_a_newline_in_the_composer() {
        let mut surface = ChatSurface::new();
        surface.validated_ready = true;
        surface.composer.set_text("one");

        assert!(surface.key(&Key::Character("j".into()), None, ModifiersState::CONTROL,));
        assert_eq!(surface.composer.text(), "one\n");
    }

    fn type_into(surface: &mut ChatSurface, text: &str) {
        for ch in text.chars() {
            let s = ch.to_string();
            surface.key(
                &Key::Character(s.as_str().into()),
                Some(&s),
                ModifiersState::empty(),
            );
        }
    }

    /// `@`, the picker answered with one document, Enter: a chip.
    fn chip_into(surface: &mut ChatSurface, title: &str) -> DocumentId {
        let document = DocumentId::new();
        type_into(surface, "@");
        assert!(surface.composer.picker_open());
        surface.composer.offer(&[(document, title)]);
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        document
    }

    fn user_message(transcript: &ChatTranscript, markdown: &str) -> koru_ai_core::Message {
        koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: transcript.chat.id,
            ordinal: transcript.messages.len() as u32,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: markdown.into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(2),
            completed_at: None,
        }
    }

    #[test]
    fn picker_keys_come_before_send_and_the_chip_is_pinned_at_send() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;

        type_into(&mut surface, "Read ");
        let document = chip_into(&mut surface, "Budget");
        assert!(
            commands.try_recv().is_err(),
            "Enter chose the document; it did not send"
        );
        type_into(&mut surface, "now");
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(
                    request.markdown,
                    format!("Read [[koru://doc/{document}]] now")
                );
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: document
                    }]
                );
            }
            _ => panic!("the second Enter sends"),
        }
        assert!(surface.composer.is_blank(), "the composer empties at send");
    }

    #[test]
    fn several_chips_are_several_refs_and_never_more_than_the_worker_takes() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let context = DocumentId::new();
        surface.set_context_document(Some((context, "Open page".into())));

        let chips: Vec<DocumentId> = (0..koru_ai_wire::MAX_CONTEXT_REFS + 1)
            .map(|i| chip_into(&mut surface, &format!("Doc {i}")))
            .collect();
        assert!(surface.send_composer());
        assert!(
            surface
                .status
                .as_deref()
                .is_some_and(|status| status.contains("16")),
            "the person is told which went: {:?}",
            surface.status
        );
        // With a page open the send waits for the page to be saved.
        surface.submit_embedded_when_durable(true);
        assert!(surface.submit_embedded_when_durable(true));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert_eq!(request.explicit_refs.len(), koru_ai_wire::MAX_CONTEXT_REFS);
                let expected: Vec<ContextRef> = chips
                    .iter()
                    .take(koru_ai_wire::MAX_CONTEXT_REFS)
                    .map(|document_id| ContextRef::Document {
                        document_id: *document_id,
                    })
                    .collect();
                assert_eq!(request.explicit_refs, expected, "chips first, in order");
            }
            _ => panic!("a send"),
        }
    }

    #[test]
    fn in_the_pane_an_earlier_chip_stays_pinned() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let earlier = DocumentId::new();
        let mut transcript = transcript();
        let message = user_message(&transcript, &format!("see [[koru://doc/{earlier}]]"));
        transcript.messages.push(message);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.composer.set_text("and now?");

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => {
                assert!(request.replace_pinned_documents);
                assert_eq!(
                    request.explicit_refs,
                    vec![ContextRef::Document {
                        document_id: earlier
                    }]
                );
            }
            _ => panic!("a send"),
        }
    }

    #[test]
    fn send_waits_for_a_provisional_dictation() {
        use koru_ui::DictationTarget;
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.composer.set_text("Hello ");
        assert!(surface.dictation_composer().begin_composition(()));
        surface
            .dictation_composer()
            .update_composition((), "still talking");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        assert!(
            commands.try_recv().is_err(),
            "nothing is sent mid-dictation"
        );
        assert_eq!(surface.composer.text(), "Hello ");

        surface
            .dictation_composer()
            .commit_composition((), "still talking");
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.markdown, "Hello still talking"),
            _ => panic!("a send once the words are text"),
        }
    }

    #[test]
    fn the_sent_message_reads_the_chip_as_a_titled_link() {
        let mut surface = ChatSurface::new();
        let (reader, commands, updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.lang = koru_i18n::Lang::En;

        type_into(&mut surface, "Summarise ");
        let document = chip_into(&mut surface, "Budget 2026");
        assert!(surface.send_composer());
        let _ = commands.recv_timeout(Duration::from_millis(50)).unwrap();

        // The just-sent card: the chip's caption, as a link to the document.
        surface.rebuild_messages(400.0);
        let pending = surface
            .message_layouts
            .iter()
            .find(|message| message.role == ChatRole::User)
            .expect("the optimistic card");
        assert_eq!(pending.display, "Summarise Budget 2026");
        assert_eq!(
            &pending.display[pending.links[0].range.clone()],
            "Budget 2026"
        );
        assert_eq!(pending.links[0].url, format!("koru://doc/{document}"));

        // The durable card in another window, which never saw the chip: the
        // fallback word, then the title once the reader finds it.
        let mut other = ChatSurface::new();
        let (reader, commands, _other_updates) = Reader::test_pair();
        other.reader = Some(reader);
        other.lang = koru_i18n::Lang::En;
        let mut transcript = transcript();
        let message = user_message(&transcript, &format!("Summarise [[koru://doc/{document}]]"));
        transcript.messages.push(message);
        other.transcript = Some(transcript);
        other.rebuild_messages(400.0);
        let durable = &other.message_layouts[0];
        assert_eq!(durable.display, "Summarise Document");
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::DocumentTitles(asked) => assert_eq!(asked, vec![document]),
            _ => panic!("the title is asked of the reader thread"),
        }
        assert!(
            other.apply_update(Update::DocumentTitles(vec![(
                document,
                "Budget 2026".into()
            )])) || other.messages_dirty
        );
        other.rebuild_messages(400.0);
        assert_eq!(other.message_layouts[0].display, "Summarise Budget 2026");
        let _ = updates;
    }

    /// **D78: *Stored “…” in Koru.* is followed by *Open***; a press asks
    /// the host to open the new document as a tab, as a document link does.
    /// A later status line takes the link with it.
    #[test]
    fn a_stored_reply_offers_open() {
        let mut surface = ChatSurface::new();
        surface.lang = koru_i18n::Lang::En;
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.composer_rect();
        let document = DocumentId::new();
        let stored = |surface: &mut ChatSurface| {
            surface.apply_update(Update::Exported(
                "Help me plan a trip to Portugal for 10 days".into(),
                Some(document),
            ));
            surface.draw(&mut Scene::new(), Affine::IDENTITY);
        };
        stored(&mut surface);
        assert_eq!(
            surface.status.as_deref(),
            Some("Stored “Help me plan a trip to Portugal for 10 days” in Koru.")
        );
        let open = surface.stored_open_rect().expect("Open after the line");
        surface.pointer = (open.x + open.w * 0.5, open.y + open.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default, "the arrow");
        assert!(surface.mouse_down(open.x + open.w * 0.5, open.y + open.h * 0.5));
        assert_eq!(
            surface.take_open_document_request(),
            Some(koru_link::KoruLink::Document {
                document: document.as_uuid(),
                node: None
            })
        );

        // Another status line takes the link with it.
        surface.status = Some("Something else".into());
        assert!(surface.stored_open_rect().is_none());
    }

    #[test]
    fn a_document_link_asks_the_host_to_open_it_and_a_web_link_does_not() {
        let mut surface = ChatSurface::new();
        let document = DocumentId::new();
        assert!(surface.follow_link(&format!("koru://doc/{document}")));
        assert_eq!(
            surface.take_open_document_request(),
            Some(koru_link::KoruLink::Document {
                document: document.as_uuid(),
                node: None
            })
        );
        assert!(surface.follow_link("https://example.com"));
        assert_eq!(surface.take_open_document_request(), None);
    }

    #[test]
    fn plain_enter_still_sends_the_composer() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.composer.set_text("hello");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.markdown, "hello"),
            _ => panic!("composer Enter must issue a turn"),
        }
    }

    #[test]
    fn pending_mutation_opens_and_exposes_approve_through_the_shared_reader() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: MessageId::new(),
            tool_name: "edit_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(2),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: None,
            live: None,
        };
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.upsert_tool_activity(activity.clone());
        surface.validated_ready = true;
        let area = surface.conversation_rect();
        surface.rebuild_messages(area.w);
        // A pending request is a card of its own, open, never folded: no
        // work view sums it up.
        assert!(surface.work_layouts.is_empty());
        let placed = surface.tool_activity_layouts[0].top;
        let row = Rect::new(
            area.x,
            area.y + placed,
            area.w,
            surface.tool_activity_layouts[0].expanded_height,
        );
        surface.mouse_move(row.x + 4.0, row.y + 4.0);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);
        let approve = tool_activity_action_rects(row)[0];

        assert!(surface.mouse_down(approve.x + 2.0, approve.y + 2.0));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ApproveTool(id) if id == activity.id
        ));

        let mut completed = activity;
        completed.status = ToolActivityStatus::Complete;
        completed.approval = ToolApproval::Approved;
        completed.started_at = Some(koru_ai_core::Timestamp::from_millis(2));
        completed.completed_at = Some(koru_ai_core::Timestamp::from_millis(2_252));
        assert_eq!(tool_activity_title(&completed), "Edit document - 2.3s");
        completed.completed_at = Some(koru_ai_core::Timestamp::from_millis(10_252));
        assert_eq!(tool_activity_title(&completed), "Edit document - 10s");

        completed.status = ToolActivityStatus::Failed;
        completed.display_target = Some("Launch plan".into());
        completed.error_message = Some("The requested node no longer exists.".into());
        assert_eq!(
            tool_activity_title(&completed),
            "Edit document — Launch plan - 10s"
        );
        let detail = tool_activity_detail(&completed);
        assert!(detail.contains("Document: Launch plan"));
        assert!(detail.contains("Failed: The requested node no longer exists."));
    }

    #[test]
    fn running_activity_keeps_a_previously_resolved_document_title() {
        let mut surface = ChatSurface::new();
        let transcript = transcript();
        let mut activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: MessageId::new(),
            tool_name: "edit_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: Some("Launch plan".into()),
            live: None,
        };
        surface.transcript = Some(transcript);
        surface.upsert_tool_activity(activity.clone());

        activity.status = ToolActivityStatus::Running;
        activity.approval = ToolApproval::Approved;
        activity.started_at = Some(koru_ai_core::Timestamp::from_millis(2));
        activity.display_target = None;
        surface.upsert_tool_activity(activity);

        assert_eq!(
            surface.transcript.as_ref().unwrap().tool_activities[0]
                .display_target
                .as_deref(),
            Some("Launch plan")
        );

        let mut failed = surface.transcript.as_ref().unwrap().tool_activities[0].clone();
        failed.status = ToolActivityStatus::Failed;
        failed.completed_at = Some(koru_ai_core::Timestamp::from_millis(3));
        failed.error_message = Some("The requested node no longer exists.".into());
        surface.upsert_tool_activity(failed);
        assert_eq!(
            surface.transcript.as_ref().unwrap().tool_activities[0].status,
            ToolActivityStatus::Failed
        );
    }

    #[test]
    fn tool_activity_is_positioned_inside_its_turn() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let user_id = MessageId::new();
        let assistant_id = MessageId::new();
        transcript.messages = vec![
            koru_ai_core::Message {
                id: user_id,
                chat_id: transcript.chat.id,
                ordinal: 0,
                role: ChatRole::User,
                state: MessageState::Complete,
                markdown: "Which workspaces can I use?".into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
            },
            koru_ai_core::Message {
                id: assistant_id,
                chat_id: transcript.chat.id,
                ordinal: 1,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                markdown: "One workspace is available.".into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(2),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(5)),
            },
        ];
        transcript.tool_activities.push(ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: assistant_id,
            tool_name: "list_workspaces".into(),
            read_only: true,
            status: ToolActivityStatus::Complete,
            approval: ToolApproval::NotRequired,
            // Placement follows the durable message id, even when timestamps
            // are insufficient or misleading.
            proposed_at: koru_ai_core::Timestamp::from_millis(0),
            started_at: Some(koru_ai_core::Timestamp::from_millis(0)),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(0)),
            result_bytes: Some(17),
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: None,
            live: None,
        });
        surface.transcript = Some(transcript);
        let conversation_width = surface.conversation_rect().w;
        surface.rebuild_messages(conversation_width);

        let user = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(user_id))
            .unwrap();
        let assistant = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(assistant_id))
            .unwrap();
        // The finished step folds into the reply's work view, in front of it.
        let work = &surface.work_layouts[0];
        assert!(user.top < work.top);
        assert!(work.top < assistant.top);
    }

    #[test]
    fn live_tool_activity_precedes_the_pending_response() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let assistant_id = MessageId::new();
        transcript.tool_activities.push(ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: transcript.chat.id,
            assistant_message_id: assistant_id,
            tool_name: "list_workspaces".into(),
            read_only: true,
            status: ToolActivityStatus::Complete,
            approval: ToolApproval::NotRequired,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: Some(koru_ai_core::Timestamp::from_millis(1)),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
            result_bytes: Some(17),
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: None,
            live: None,
        });
        surface.transcript = Some(transcript);
        surface.pending_user = Some("What can I access?".into());
        surface.pending_assistant_message_id = Some(assistant_id);
        let conversation_width = surface.conversation_rect().w;
        surface.rebuild_messages(conversation_width);

        let pending = surface
            .message_layouts
            .iter()
            .find(|message| message.id == Some(assistant_id))
            .unwrap();
        assert!(surface.work_layouts[0].top < pending.top);
    }

    /// A tool waiting for approval waits for the person: nothing approves
    /// it on its own, whatever was approved before it in the chat.
    #[test]
    fn a_waiting_tool_is_never_approved_on_its_own() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);

        let activity = ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: chat,
            assistant_message_id: MessageId::new(),
            tool_name: "create_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(1),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: None,
            live: None,
        };
        surface.upsert_tool_activity(activity);
        assert!(commands.recv_timeout(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn reasoning_choices_are_capability_driven() {
        use koru_ai_core::ReasoningLevel;

        assert!(choices_for_reasoning(&ReasoningControl::Unsupported).is_empty());
        assert_eq!(
            choices_for_reasoning(&ReasoningControl::Toggle { default_on: true }),
            vec![
                ReasoningChoice::Default,
                ReasoningChoice::Off,
                ReasoningChoice::On,
            ]
        );
        assert_eq!(
            choices_for_reasoning(&ReasoningControl::Levels {
                values: vec![ReasoningLevel::Low, ReasoningLevel::High],
                default: Some(ReasoningLevel::Low),
                supports_off: true,
            }),
            vec![
                ReasoningChoice::Default,
                ReasoningChoice::Off,
                ReasoningChoice::Level(ReasoningLevel::Low),
                ReasoningChoice::Level(ReasoningLevel::High),
            ]
        );
    }

    #[test]
    fn provider_readiness_needs_the_provider_specific_prerequisites() {
        let openai = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse("openai").unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: Some("gpt-5.1".into()),
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        assert!(provider_ready(&openai));
        let mut missing_key = openai;
        missing_key.openai.credential_configured = false;
        assert!(!provider_ready(&missing_key));

        let koru = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::KORU).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        assert!(provider_ready(&koru));
        assert_eq!(selected_setup_provider(&koru), SetupProvider::Koru);
    }

    #[test]
    fn an_agent_chat_keeps_its_folder_without_a_composer_chooser() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.composer_rect();
        assert!(surface.folder_button().is_none());
        assert!(surface.take_working_folder_request().is_none());
        assert!(
            surface
                .transcript
                .as_ref()
                .unwrap()
                .chat
                .agent_working_folder
                .is_none()
        );
        surface
            .transcript
            .as_mut()
            .unwrap()
            .chat
            .agent_working_folder = Some("D:/work/koru".into());
        surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .unwrap()
                .chat
                .agent_working_folder
                .as_deref(),
            Some("D:/work/koru"),
        );
        assert!(surface.folder_button().is_none());
    }

    #[test]
    fn the_composer_has_no_folder_footer_in_either_position() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        let empty = surface.composer_rect();
        assert!(surface.composer_footer_rect().is_none());
        assert!(surface.folder_button().is_none());
        surface.pending_user = Some("hello".into());
        let active = surface.composer_rect();
        assert!(active.y > empty.y);
        assert_eq!(
            active.y + active.h,
            surface.viewport.h - COMPOSER_BOTTOM_MARGIN
        );
        let shadow = scene::panel_bounds(
            active.x,
            active.y,
            active.w,
            active.h,
            scene::Elevation::Raised,
        );
        assert!(shadow.y + shadow.h <= surface.viewport.h);
        assert!(surface.composer_footer_rect().is_none());
    }
    #[test]
    fn a_running_agent_check_is_followed_up_while_the_picker_shows_it() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.settings = Some(ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OPENAI).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: vec![koru_ai_wire::AgentProviderView {
                id: AiProviderId::parse("agent-claude").unwrap(),
                label: "Claude".into(),
                steers: false,
            }],
        });
        let claude = AiProviderId::parse("agent-claude").unwrap();
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::Checking);
        let start = Instant::now();

        // Nothing shows the agents: nothing is asked.
        surface.follow_agent_checks(start + Duration::from_secs(5));
        assert!(commands.try_recv().is_err());

        // The picker shows them and a check is running: asked, then not
        // again until the follow-up interval has passed.
        surface.chat_model_open = true;
        surface.follow_agent_checks(start + Duration::from_secs(5));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Validate(provider) if provider == claude
        ));
        surface.follow_agent_checks(Instant::now());
        assert!(commands.try_recv().is_err());

        // A settled check stops the follow-up.
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::CheckFailed);
        surface.follow_agent_checks(Instant::now() + Duration::from_secs(5));
        assert!(commands.try_recv().is_err());
    }

    /// Agent-models D8: every agent's list is asked for when settings
    /// arrive, so the picker opens on it; one ask is out at a time; a list
    /// on screen stays through a failed or pending ask; and an agent that
    /// turns ready while the picker is open is asked again.
    #[test]
    fn agent_models_are_asked_for_ahead_of_the_picker_and_kept_meanwhile() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let claude = AiProviderId::parse("agent-claude").unwrap();
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OPENAI).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: vec![koru_ai_wire::AgentProviderView {
                id: claude.clone(),
                label: "Claude".into(),
                steers: false,
            }],
        };
        let discover = |commands: &std::sync::mpsc::Receiver<Command>| {
            std::iter::from_fn(|| commands.try_recv().ok())
                .filter(|command| matches!(command, Command::DiscoverModels(_)))
                .count()
        };

        surface.apply_update(Update::Settings(settings));
        assert_eq!(discover(&commands), 1, "asked as settings arrive");
        assert_eq!(
            surface.agent_models.get(&claude),
            Some(&agents::AgentModels::Loading)
        );
        surface.prefetch_agent_models();
        assert_eq!(discover(&commands), 0, "one ask out at a time");

        let listed = vec![koru_ai_wire::AgentModelView {
            id: "opus".into(),
            group: "claude".into(),
            name: Some("Claude Opus 4.7".into()),
            description: None,
            group_name: None,
            legacy: false,
            reasoning: None,
            context: None,
            fast: None,
        }];
        surface.apply_update(Update::AgentModels(koru_ai_wire::AgentModelsResponse {
            provider: claude.clone(),
            modes: Vec::new(),
            default_mode: None,
            levels_per_turn: false,
            models_per_turn: false,
            models: listed.clone(),
        }));
        surface.prefetch_agent_models();
        assert_eq!(discover(&commands), 1, "asked again once answered");
        assert_eq!(
            surface.agent_models.get(&claude),
            Some(&agents::AgentModels::Listed(listed.clone())),
            "the list stays while the answer is on its way"
        );
        surface.apply_update(Update::AgentModelsFailed(claude.clone()));
        assert_eq!(
            surface.agent_models.get(&claude),
            Some(&agents::AgentModels::Listed(listed)),
            "a failed ask does not take a list off the screen"
        );
        // A list the host is still building: what is on screen stays, and
        // the next open asks again.
        surface.prefetch_agent_models();
        assert_eq!(discover(&commands), 1);
        surface.apply_update(Update::AgentModelsPending(claude.clone()));
        assert!(matches!(
            surface.agent_models.get(&claude),
            Some(agents::AgentModels::Listed(_))
        ));

        surface.chat_model_open = true;
        surface
            .agent_status
            .insert(claude.clone(), AgentStatus::Checking);
        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: claude.clone(),
            status: ProviderValidationStatus::Ready,
            models: Vec::new(),
            selected_model: None,
            failure: None,
        }));
        assert_eq!(discover(&commands), 1, "turning ready asks again");
    }

    #[test]
    fn koru_card_selects_without_creating_a_chat_and_save_selects_the_provider() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.lang = koru_i18n::Lang::En;
        let cards = surface.provider_cards();
        assert_eq!(cards.len(), 4);
        assert!(surface.setup_mouse_down(cards[0].x + 8.0, cards[0].y + 8.0));
        assert_eq!(surface.setup_provider, SetupProvider::Koru);
        assert!(surface.transcript.is_none());
        assert!(commands.recv_timeout(Duration::from_millis(10)).is_err());

        let save = surface.setup_save_button();
        assert!(surface.setup_mouse_down(save.x + 4.0, save.y + 4.0));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::SelectProvider(request) => {
                assert_eq!(request.provider.as_str(), AiProviderId::KORU);
            }
            _ => panic!("expected SelectProvider"),
        }
        assert!(surface.transcript.is_none());
        assert!(surface.web_search_button().is_none());

        surface.settings = Some(ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::KORU).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        });
        let selection = surface.current_selection().expect("koru selection");
        assert_eq!(selection.provider.as_str(), AiProviderId::KORU);
        assert_eq!(selection.requested_model, AiProviderId::KORU_HOSTED_MODEL);
    }

    /// Claude's models as its listing carries them: `opus` with levels and
    /// no stated default, `sonnet` with fewer, `haiku` with none.
    fn claude_listing(surface: &mut ChatSurface) {
        let levels = |values: Vec<ReasoningLevel>| {
            Some(ReasoningControl::Levels {
                values,
                default: None,
                supports_off: false,
            })
        };
        let model = |id: &str, reasoning| koru_ai_wire::AgentModelView {
            id: id.into(),
            group: "claude".into(),
            name: None,
            description: None,
            group_name: None,
            legacy: false,
            reasoning,
            context: None,
            fast: None,
        };
        surface.apply_update(Update::AgentModels(koru_ai_wire::AgentModelsResponse {
            provider: AiProviderId::parse("agent-claude").unwrap(),
            modes: Vec::new(),
            default_mode: None,
            levels_per_turn: false,
            models_per_turn: false,
            models: vec![
                model(
                    "opus",
                    levels(vec![
                        ReasoningLevel::Low,
                        ReasoningLevel::High,
                        ReasoningLevel::Max,
                    ]),
                ),
                model(
                    "sonnet",
                    levels(vec![ReasoningLevel::Low, ReasoningLevel::High]),
                ),
                model("haiku", None),
            ],
        }));
    }

    /// **Rung 4: an agent model's levels in today's effort section.** A new
    /// chat on Claude's Opus shows the levels its listing carries; on
    /// *Default* the chip names no level (the agent decides) and the label
    /// reads *Default*; a chosen level is kept on the chat and named.
    #[test]
    fn an_agent_models_levels_show_and_the_chip_names_a_chosen_one() {
        let (mut surface, commands, _updates) = unmade_claude_chat();
        surface.lang = koru_i18n::Lang::En;
        claude_listing(&mut surface);
        let claude = AiProviderId::parse("agent-claude").unwrap();
        assert!(surface.choose_agent_model(claude.clone(), "opus".into()));
        while commands.try_recv().is_ok() {}
        assert_eq!(
            effort::stops(surface.effort_control().unwrap()).len(),
            3,
            "opus's own levels"
        );
        assert_eq!(
            surface.effective_reasoning(),
            Some(ReasoningChoice::Default)
        );
        assert_eq!(surface.chip_label(), "Claude · Opus");
        assert!(surface.choose_effort(ReasoningChoice::Level(ReasoningLevel::Max)));
        assert!(
            commands.try_recv().is_err(),
            "kept on the chat not made yet"
        );
        assert_eq!(surface.chip_label(), "Claude · Opus · Max");

        // Haiku offers none: no slider, and the level goes back to Default.
        assert!(surface.choose_agent_model(claude.clone(), "haiku".into()));
        assert!(surface.effort_control().is_none());
        assert_eq!(
            surface
                .transcript
                .as_ref()
                .unwrap()
                .chat
                .current_model
                .default_reasoning,
            ReasoningChoice::Default
        );
    }

    /// **D18: a new model keeps the chat's level when it offers it.**
    #[test]
    fn picking_another_model_keeps_a_level_it_offers() {
        let (mut surface, _commands, _updates) = unmade_claude_chat();
        claude_listing(&mut surface);
        let claude = AiProviderId::parse("agent-claude").unwrap();
        surface.choose_agent_model(claude.clone(), "opus".into());
        surface.choose_effort(ReasoningChoice::Level(ReasoningLevel::High));
        surface.choose_agent_model(claude.clone(), "sonnet".into());
        let level = |surface: &ChatSurface| {
            surface
                .transcript
                .as_ref()
                .unwrap()
                .chat
                .current_model
                .default_reasoning
        };
        assert_eq!(
            level(&surface),
            ReasoningChoice::Level(ReasoningLevel::High)
        );
        surface.choose_agent_model(claude.clone(), "opus".into());
        surface.choose_effort(ReasoningChoice::Level(ReasoningLevel::Max));
        surface.choose_agent_model(claude, "sonnet".into());
        assert_eq!(
            level(&surface),
            ReasoningChoice::Default,
            "sonnet has no max"
        );
    }

    /// **S5, against a v19 host: after an agent chat's first message its
    /// level only shows** — the agent started at it, and such a host would
    /// ignore a level sent with a message (D70). A press on its effort strip
    /// says why instead.
    #[test]
    fn an_agent_chats_level_is_read_only_after_its_first_message() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.settings = Some(unchosen_settings(&["agent-claude"]));
        claude_listing(&mut surface);
        let mut chat = transcript();
        chat.chat.current_model = ModelSelection {
            provider: AiProviderId::parse("agent-claude").unwrap(),
            requested_model: "opus".into(),
            default_reasoning: ReasoningChoice::Level(ReasoningLevel::High),
        };
        chat.messages.push(koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: chat.chat.id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "hi".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
        });
        surface.transcript = Some(chat);
        surface.composer_rect();
        assert!(surface.open_model_picker());
        while commands.try_recv().is_ok() {}
        let control = surface.effort_control().cloned().unwrap();
        let low = surface
            .picker_segment_rect(effort::stops(&control)[0])
            .expect("the level still shows");
        assert!(surface.mouse_down(low.x + low.w * 0.5, low.y + low.h * 0.5));
        assert_eq!(
            surface.status.as_deref(),
            Some(koru_i18n::t(
                surface.lang,
                koru_i18n::Key::AiEffortLockedAfterStart
            ))
        );
        assert!(
            !std::iter::from_fn(|| commands.try_recv().ok())
                .any(|command| matches!(command, Command::UpdateModel(..)))
        );
    }

    /// **D70: where the agent's host takes a level per message, a begun
    /// chat's level changes from its next message**, as Fast does; while a
    /// reply runs the strip says so. The *1M context* pill stays the
    /// start's against a v21 host: that is a model, and such a host keeps a
    /// live session's model. A v22 host's listing unlocks it.
    #[test]
    fn a_begun_agent_chats_level_changes_from_its_next_message() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.settings = Some(unchosen_settings(&["agent-claude"]));
        claude_listing(&mut surface);
        let mut chat = transcript();
        chat.chat.current_model = ModelSelection {
            provider: AiProviderId::parse("agent-claude").unwrap(),
            requested_model: "opus".into(),
            default_reasoning: ReasoningChoice::Level(ReasoningLevel::High),
        };
        chat.messages.push(koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: chat.chat.id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "hi".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(1)),
        });
        surface.transcript = Some(chat);
        assert!(surface.effort_locked(), "a v19 host keeps the lock");
        // The listing says the host takes a level per message.
        let mut listing = koru_ai_wire::AgentModelsResponse {
            provider: AiProviderId::parse("agent-claude").unwrap(),
            modes: Vec::new(),
            default_mode: None,
            levels_per_turn: true,
            models_per_turn: false,
            models: surface
                .listed_agent_models(&AiProviderId::parse("agent-claude").unwrap())
                .to_vec(),
        };
        surface.apply_update(Update::AgentModels(listing.clone()));
        assert!(!surface.effort_locked());
        assert!(surface.model_locked(), "the model stays the start's");
        // A v22 host takes a model per message too: the model unlocks.
        let mut v22 = listing.clone();
        v22.models_per_turn = true;
        surface.apply_update(Update::AgentModels(v22));
        assert!(!surface.model_locked());
        surface.apply_update(Update::AgentModels(listing.clone()));
        assert!(
            surface.model_locked(),
            "a v21 host's listing locks it again"
        );

        surface.composer_rect();
        assert!(surface.open_model_picker());
        while commands.try_recv().is_ok() {}
        // A reply is running: the level is the next message's.
        surface.attempt = Some(InferenceAttemptId::new());
        let low = surface
            .picker_segment_rect(ReasoningChoice::Level(ReasoningLevel::Low))
            .expect("the strip shows Low");
        assert!(surface.mouse_down(low.x + low.w * 0.5, low.y + low.h * 0.5));
        let sent =
            std::iter::from_fn(|| commands.try_recv().ok()).find_map(|command| match command {
                Command::UpdateModel(_, request) => Some(request.model.default_reasoning),
                _ => None,
            });
        assert_eq!(sent, Some(ReasoningChoice::Level(ReasoningLevel::Low)));
        let note = surface.copy_feedback.label().unwrap_or_default().to_owned();
        assert!(
            note.contains(koru_i18n::t(
                surface.lang,
                effort::word_key(ReasoningChoice::Level(ReasoningLevel::Low))
            )),
            "{note}"
        );
        assert_ne!(
            surface.status.as_deref(),
            Some(koru_i18n::t(
                surface.lang,
                koru_i18n::Key::AiEffortLockedAfterStart
            ))
        );

        // The listing of a v19 host locks it again.
        listing.levels_per_turn = false;
        surface.apply_update(Update::AgentModels(listing));
        assert!(surface.effort_locked());
    }

    /// **An agent's notice is a grey line in front of its reply** (koru-ai
    /// D69): between the question and the answer, never in the answer's
    /// text; one that comes live is kept once.
    #[test]
    fn an_agent_notice_is_a_line_in_front_of_its_reply() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        let mut chat = transcript();
        let message = |ordinal: u32, role: ChatRole, markdown: &str| koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: chat.chat.id,
            ordinal,
            role,
            state: MessageState::Complete,
            markdown: markdown.into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        };
        let question = message(0, ChatRole::User, "hi");
        let reply = message(1, ChatRole::Assistant, "Hello.");
        let notice = |text: &str| koru_ai_core::AgentNotice {
            id: koru_ai_core::AgentNoticeId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: chat.chat.id,
            assistant_message_id: reply.id,
            text: text.into(),
            text_offset: Some(0),
            created_at: koru_ai_core::Timestamp::from_millis(1),
        };
        chat.notices
            .push(notice("Hook failed: exited with code 127"));
        let live = notice("Indexing");
        chat.messages = vec![question.clone(), reply.clone()];
        surface.transcript = Some(chat);
        surface.rebuild_messages(600.0);
        let top_of = |surface: &ChatSurface, id: MessageId| {
            surface
                .message_layouts
                .iter()
                .find(|layout| layout.id == Some(id))
                .unwrap()
                .top
        };
        assert_eq!(surface.notice_layouts.len(), 1);
        let line = surface.notice_layouts[0].top;
        assert!(top_of(&surface, question.id) < line);
        assert!(line < top_of(&surface, reply.id));
        assert!(
            !surface
                .message_layouts
                .iter()
                .any(|layout| layout.display.contains("Hook"))
        );

        for _ in 0..2 {
            surface.apply_update(Update::Event(koru_ai_wire::StreamEvent {
                version: 1,
                sequence: 1,
                event: PublicAiEvent::AgentNotice {
                    attempt_id: live.attempt_id,
                    notice: live.clone(),
                },
            }));
        }
        assert_eq!(surface.transcript.as_ref().unwrap().notices.len(), 2);
        surface.rebuild_messages(600.0);
        let tops: Vec<f32> = surface.notice_layouts.iter().map(|line| line.top).collect();
        assert_eq!(tops.len(), 2);
        assert!(tops[0] < tops[1], "in the order they came");
        assert!(tops[1] < top_of(&surface, reply.id));
    }

    /// **Where an agent chat moved to another of its agent's models, a grey
    /// line says so in front of the first reply on it**, read from the model
    /// each reply recorded: none in front of the first reply, none where
    /// the model stayed, and one each time it moved.
    #[test]
    fn a_model_switch_is_a_line_in_front_of_the_first_reply_on_it() {
        let mut surface = ChatSurface::new();
        surface.lang = koru_i18n::Lang::En;
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        claude_listing(&mut surface);
        let claude = AiProviderId::parse("agent-claude").unwrap();
        let mut chat = transcript();
        chat.chat.current_model = ModelSelection {
            provider: claude.clone(),
            requested_model: "sonnet".into(),
            default_reasoning: ReasoningChoice::Default,
        };
        let chat_id = chat.chat.id;
        let message = |ordinal: u32, model: Option<&str>| koru_ai_core::Message {
            id: MessageId::new(),
            chat_id,
            ordinal,
            role: if model.is_some() {
                ChatRole::Assistant
            } else {
                ChatRole::User
            },
            state: MessageState::Complete,
            markdown: "words".into(),
            origin_message_id: None,
            provider: model.map(|_| claude.clone()),
            requested_model: model.map(Into::into),
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        };
        chat.messages = vec![
            message(0, None),
            message(1, Some("opus")),
            message(2, None),
            message(3, Some("opus")),
            message(4, None),
            message(5, Some("sonnet")),
        ];
        let moved = chat.messages[5].id;
        surface.transcript = Some(chat);
        assert_eq!(
            surface.model_switches(),
            [(moved, "Switched to Sonnet".to_owned())],
            "the listing names none: its id formatted"
        );
        surface.rebuild_messages(600.0);
        assert_eq!(surface.notice_layouts.len(), 1);
        let line = surface.notice_layouts[0].top;
        let top_of = |id: MessageId| {
            surface
                .message_layouts
                .iter()
                .find(|layout| layout.id == Some(id))
                .unwrap()
                .top
        };
        let asked = surface.transcript.as_ref().unwrap().messages[4].id;
        assert!(top_of(asked) < line && line < top_of(moved));

        // Not an agent chat: no line, whatever the replies recorded.
        let transcript = surface.transcript.as_mut().unwrap();
        transcript.chat.current_model.provider = AiProviderId::parse(AiProviderId::OPENAI).unwrap();
        assert!(surface.model_switches().is_empty());
    }

    /// **D19: a refused level is said in the person's language.**
    #[test]
    fn a_refused_level_reads_in_the_persons_language() {
        let mut surface = ChatSurface::new();
        surface.lang = koru_i18n::Lang::De;
        surface.apply_update(Update::Event(koru_ai_wire::StreamEvent {
            version: 1,
            sequence: 1,
            event: PublicAiEvent::Failed {
                attempt_id: InferenceAttemptId::new(),
                error: koru_ai_core::AiError::new(
                    AiErrorClass::ModelUnavailable,
                    koru_ai_core::AGENT_REASONING_REJECTED,
                    false,
                ),
                at: koru_ai_core::Timestamp::from_millis(1),
            },
        }));
        assert_eq!(
            surface.status.as_deref(),
            Some("Der Agent hat diese Denkstufe nicht übernommen")
        );
    }

    #[test]
    fn the_chip_reads_the_effort_and_a_stop_sets_the_chats_default() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.lang = koru_i18n::Lang::En;
        let mut chat = transcript();
        chat.chat.current_model.requested_model = "gpt-5.5".into();
        chat.reasoning_control = Some(ReasoningControl::Levels {
            values: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
            ],
            default: Some(ReasoningLevel::Medium),
            supports_off: false,
        });
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();
        assert_eq!(surface.chip_label(), "gpt-5.5 · Medium");

        let chip = surface.chat_model_button();
        assert!(surface.mouse_down(chip.x + 2.0, chip.y + 2.0));
        let control = surface.effort_control().cloned().unwrap();
        let high = surface
            .picker_segment_rect(ReasoningChoice::Level(ReasoningLevel::High))
            .expect("a model with levels has an effort strip");
        assert!(surface.mouse_down(high.x + high.w * 0.5, high.y + high.h * 0.5));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateModel(id, request) => {
                assert_eq!(id, chat_id);
                assert_eq!(request.model.requested_model, "gpt-5.5");
                assert_eq!(
                    request.model.default_reasoning,
                    ReasoningChoice::Level(ReasoningLevel::High)
                );
            }
            _ => panic!("expected the model update"),
        }
        let mut updated = surface.transcript.clone().unwrap().chat;
        updated.current_model.default_reasoning = ReasoningChoice::Level(ReasoningLevel::High);
        surface.apply_update(Update::ModelUpdated(updated, Some(control)));
        assert_eq!(surface.chip_label(), "gpt-5.5 · High");
        assert!(
            surface.chat_model_open,
            "the footer stays for the next step"
        );

        // Sending carries no per-turn override: the chat's setting is the effort.
        surface.composer.set_text("hello");
        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.reasoning_override, None),
            _ => panic!("expected send"),
        }
    }

    #[test]
    fn a_koru_chat_folds_its_thinking_toggle_into_the_chip() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        surface.lang = koru_i18n::Lang::En;
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse(AiProviderId::KORU).unwrap();
        chat.chat.current_model.requested_model = AiProviderId::KORU_HOSTED_MODEL.into();
        chat.reasoning_control = Some(ReasoningControl::Toggle { default_on: true });
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer_rect();
        assert_eq!(
            surface.chip_label(),
            format!("{} · On", AiProviderId::KORU_HOSTED_MODEL_LABEL)
        );
        assert!(surface.web_search_button().is_none());

        let chip = surface.chat_model_button();
        assert!(surface.mouse_down(chip.x + 2.0, chip.y + 2.0));
        assert!(
            surface.chat_model_open,
            "the chip opens even with no model list"
        );
        let off = surface
            .picker_segment_rect(ReasoningChoice::Off)
            .expect("the toggle is a strip of two");
        assert!(surface.mouse_down(off.x + off.w * 0.5, off.y + off.h * 0.5));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::UpdateModel(_, request) => {
                assert_eq!(request.model.default_reasoning, ReasoningChoice::Off);
            }
            _ => panic!("expected the model update"),
        }
    }

    #[test]
    fn selected_ollama_catalog_refresh_revokes_and_restores_readiness() {
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OLLAMA).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: None,
                reasoning: ReasoningChoice::Default,
                credential_configured: false,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: Some("qwen3.5:4b".into()),
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        let mut surface = ChatSurface::new();
        surface.apply_settings_fields(&settings);
        surface.settings = Some(settings);
        surface.setup_provider = SetupProvider::Ollama;
        surface.setup_open = true;
        surface.validated_ready = true;
        surface.transcript = Some(transcript());

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::NoModels,
            models: Vec::new(),
            selected_model: None,
            failure: None,
        }));
        assert!(!surface.validated_ready);
        assert!(surface.setup_open);
        assert!(surface.validation_retryable());
        assert!(surface.ollama_models.is_empty());

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::Ready,
            models: vec!["qwen3.5:4b".into(), "gemma4:e4b".into()],
            selected_model: None,
            failure: None,
        }));
        assert!(surface.validated_ready);
        assert!(!surface.setup_open);
        assert_eq!(
            surface.ollama_models,
            ["qwen3.5:4b".to_owned(), "gemma4:e4b".to_owned()]
        );
    }

    #[test]
    fn previewing_another_provider_does_not_replace_selected_readiness() {
        let settings = ProviderSettingsResponse {
            selected_provider: Some(AiProviderId::parse(AiProviderId::OPENAI).unwrap()),
            openai: koru_ai_wire::OpenAiSettingsView {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Default,
                credential_configured: true,
            },
            ollama: koru_ai_wire::OllamaSettingsView {
                endpoint: "http://localhost:11434".into(),
                model: Some("qwen3.5:4b".into()),
                reasoning: ReasoningChoice::Default,
            },
            agents: Vec::new(),
        };
        let mut surface = ChatSurface::new();
        surface.apply_settings_fields(&settings);
        surface.settings = Some(settings);
        surface.setup_provider = SetupProvider::Ollama;
        surface.setup_open = true;
        surface.validated_ready = true;

        surface.apply_update(Update::Validation(ProviderValidationResponse {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).unwrap(),
            status: ProviderValidationStatus::Ready,
            models: vec!["qwen3.5:4b".into()],
            selected_model: None,
            failure: None,
        }));

        assert!(surface.validated_ready);
        assert!(surface.setup_open);
        assert_eq!(
            surface.status.as_deref(),
            Some("Provider ready. Save & check to use it.")
        );
    }

    #[test]
    fn ollama_pull_can_cancel_before_first_progress_and_retry_after_terminal() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface
            .ollama_model
            .set_text(&mut surface.engine, "qwen3.5:4b");

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Pull(PullOllamaModelRequest { model }) if model == "qwen3.5:4b"
        ));
        assert!(surface.pull_active);
        assert!(surface.waiting);
        assert_eq!(surface.pull_status.as_deref(), Some("Starting qwen3.5:4b…"));

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::CancelPull
        ));
        assert_eq!(surface.pull_status.as_deref(), Some("Stopping model pull…"));

        surface.apply_update(Update::Pull(ModelPullEvent::Cancelled {
            model: "qwen3.5:4b".into(),
        }));
        assert!(!surface.pull_active);
        assert!(!surface.waiting);
        assert_eq!(
            surface.pull_status.as_deref(),
            Some("Stopped pulling qwen3.5:4b")
        );

        assert!(surface.pull_ollama_model());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Pull(PullOllamaModelRequest { model }) if model == "qwen3.5:4b"
        ));
        assert!(surface.pull_active);
    }

    #[test]
    fn openai_discovery_prefers_a_recent_text_model() {
        let models = vec![
            "text-embedding-3-large".into(),
            "gpt-5.2-realtime".into(),
            "gpt-5.1".into(),
            "gpt-5.6".into(),
        ];
        assert_eq!(preferred_openai_model(&models), Some("gpt-5.6"));
        assert!(!likely_openai_text_model("gpt-5.2-realtime"));
        assert!(!likely_openai_text_model("text-embedding-3-large"));
    }

    #[test]
    fn openai_picker_does_not_hide_gpt_models_behind_o_models() {
        let mut models = vec![
            "o4-mini".to_owned(),
            "o3".to_owned(),
            "gpt-5.5".to_owned(),
            "gpt-5.6".to_owned(),
            "gpt-4.1".to_owned(),
        ];
        sort_model_options(&mut models);
        assert_eq!(&models[..3], ["gpt-5.6", "gpt-5.5", "gpt-4.1"]);
    }

    /// **The title row is gone**: nothing is drawn or hit at the top of the
    /// surface, whose conversation starts a margin below the top — and no
    /// header buttons are there either.
    #[test]
    fn the_chat_title_is_neither_drawn_nor_hit() {
        let mut surface = ChatSurface::new();
        let mut chat = transcript();
        chat.messages.push(user_message(&chat, "hello"));
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
        let top = surface.viewport.y + 7.0 + BUTTON_H * 0.5;
        // Where the title's words were, and where the header's *Chats* was.
        for x in [
            surface.viewport.x + surface.viewport.w * 0.5,
            surface.viewport.x + PAD + 4.0,
        ] {
            assert!(!surface.mouse_double_click(x, top));
            assert_eq!(surface.control_hover_at(x, top), None);
            assert_eq!(surface.focus, Focus::Composer);
        }
        let mut pane = ChatSurface::new();
        pane.transcript = Some(transcript());
        assert_eq!(pane.conversation_rect().y, pane.viewport.y + PAD);
    }

    /// **The private mark is hidden for now** (the user, 2026-11-03:
    /// `koru_features::PRIVATE_CHAT_BUTTON_SHOWN`): the composer's toolbar
    /// has none, a press on the plus menu makes nothing private, and web
    /// search lives in that menu. A chat that is already private still
    /// takes the service's answer and stays private.
    #[test]
    fn the_private_mark_is_hidden_and_a_private_chat_stays_private() {
        assert!(!koru_features::PRIVATE_CHAT_BUTTON_SHOWN);
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let mut transcript = transcript();
        transcript.chat.current_model.requested_model = "gpt-5.5".into();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;

        surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
        assert_eq!(surface.privacy_button(), None);
        let toolbar = surface.composer_toolbar_rect();
        let plus = surface.plus_button();
        assert_eq!(plus.x, toolbar.x, "the context button keeps the left edge");
        assert!(surface.web_search_button().is_none());
        surface.open_plus_menu();
        assert!(
            surface
                .plus_menu
                .as_ref()
                .unwrap()
                .items()
                .iter()
                .any(|item| matches!(item, composer_plus::PlusItem::SearchWeb(_, true)))
        );
        assert!(surface.web_search_armed);
        assert!(
            !matches!(
                commands.recv_timeout(Duration::from_millis(50)),
                Ok(Command::UpdatePrivacy(..))
            ),
            "nothing is made private from the composer"
        );

        let mut chat = surface.transcript.as_ref().unwrap().chat.clone();
        chat.private = true;
        assert!(surface.apply_update(Update::PrivacyUpdated(chat)));
        assert!(surface.transcript.as_ref().unwrap().chat.private);
        assert!(surface.chats[0].private);
    }

    #[test]
    fn desktop_controls_keep_the_arrow_while_editable_text_uses_the_ibeam() {
        let mut surface = ChatSurface::new();
        surface.transcript = Some(transcript());
        surface.validated_ready = true;

        surface.composer_rect();
        let send = surface.send_button();
        surface.mouse_move(send.x + 4.0, send.y + send.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);

        let text = surface.composer_text_rect();
        surface.mouse_move(text.x + 8.0, text.y + text.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Text);

        surface.setup_open = true;
        let clear = surface.clear_history_button();
        surface.mouse_move(clear.x + 4.0, clear.y + clear.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default);
        let model = surface.field_rects()[1];
        surface.mouse_move(model.x + 8.0, model.y + model.h * 0.5);
        assert_eq!(surface.cursor_icon(), CursorIcon::Text);
    }

    /// **`/usage` is a message like any other** (koru-ai D121): Koru no
    /// longer reads it at send, so it goes to the chat's model or agent,
    /// whose answer shows in the conversation.
    #[test]
    fn usage_is_sent_to_the_agent_as_a_message() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        let chat_id = transcript.chat.id;
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.validated_ready = true;

        assert!(surface.key(
            &Key::Character("/".into()),
            Some(" /usage "),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));

        let request = loop {
            match commands.recv_timeout(Duration::from_millis(200)).unwrap() {
                Command::Send(request) => break request,
                Command::Usage(_) => panic!("/usage is no longer read by Koru"),
                _ => continue,
            }
        };
        assert_eq!(request.chat_id, chat_id);
        assert_eq!(request.markdown, "/usage");
        assert!(surface.composer.is_blank());
        assert_eq!(surface.pending_user.as_deref(), Some("/usage"));
    }

    #[test]
    fn destructive_confirmation_defaults_to_cancel() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.open_confirmation(DestructiveAction::ClearHistory);

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert!(commands.try_recv().is_err());

        surface.open_confirmation(DestructiveAction::ClearHistory);
        assert!(surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(),));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ClearHistory(None)
        ));
    }

    #[test]
    fn usage_copy_distinguishes_unavailable_measurements_from_zero() {
        let chat_id = ChatId::new();
        let report = UsageReport {
            chat_id: Some(chat_id),
            current_chat: Some(UsageTotals {
                attempts: 1,
                context_payload_bytes: 0,
                ..UsageTotals::default()
            }),
            groups: Vec::new(),
        };

        let text = format_usage_report(&report);
        assert!(text.starts_with("Usage — private and local\n"));
        assert!(text.contains("\n\nCurrent chat\n"));
        assert!(text.contains("request unavailable B"));
        assert!(text.contains("Monetary cost is not estimated."));
        assert!(!text.contains('$'));
    }

    #[test]
    fn generated_image_blocks_accept_the_documented_fenced_shape() {
        let markdown = r#"```json
{
  "kind": "generated_image",
  "url": "https://cdn.example.com/generated/image.png",
  "alt": "A generated Koru image."
}
```"#;
        let image = generated_image_spec(markdown).expect("generated image block");
        assert_eq!(image.url, "https://cdn.example.com/generated/image.png");
        assert_eq!(image.alt, "A generated Koru image.");
    }

    #[test]
    fn non_web_image_and_link_targets_are_refused() {
        assert!(!is_web_url("file:///C:/secret.png"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(is_web_url("https://example.com/image.png"));
        assert!(open_web_link("https://example.com/source"));
        // The allow-list answers on the calling thread, before any thread
        // is started for the shell.
        assert!(!open_web_link("javascript:alert(1)"));
        assert!(!open_web_link("file:///C:/secret.png"));
    }

    #[test]
    fn the_composer_accepts_a_provisional_multiline_dictation() {
        let mut surface = ChatSurface::new();
        use koru_ui::DictationTarget;
        let composer = surface.dictation_composer();
        assert!(composer.begin_composition(()));
        composer.update_composition((), "Draft a response");
        assert_eq!(composer.text(), "", "the partial is provisional");
        composer.commit_composition((), "Draft a response\nwith sources");
        assert_eq!(composer.text(), "Draft a response\nwith sources");
    }

    fn press(surface: &mut ChatSurface, key: NamedKey) -> bool {
        surface.key(&Key::Named(key), None, ModifiersState::empty())
    }

    /// **Escape twice stops the work in progress**; once only says how. The
    /// second press has to come inside the window, and a popover open over
    /// the composer takes the press first.
    #[test]
    fn escape_twice_stops_the_turn_and_once_only_says_how() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        let attempt = InferenceAttemptId::new();
        surface.attempt = Some(attempt);
        let hint = koru_i18n::t(surface.lang, koru_i18n::Key::AiPressEscAgainToStop);

        assert!(press(&mut surface, NamedKey::Escape));
        assert!(
            commands.try_recv().is_err(),
            "one Escape never stops a turn"
        );
        assert_eq!(surface.footer_line(), Some(hint), "the hint says how");
        assert!(surface.maintenance_deadline().is_some(), "and will go away");
        assert!(press(&mut surface, NamedKey::Escape));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Cancel(cancelled) => assert_eq!(cancelled, attempt),
            _ => panic!("the second Escape cancels the attempt"),
        }
        assert_ne!(surface.footer_line(), Some(hint));

        // Past the window the hint goes, and the next Escape starts over.
        assert!(press(&mut surface, NamedKey::Escape));
        surface.stop_armed_at =
            Instant::now().checked_sub(STOP_ESCAPE_WINDOW + Duration::from_millis(10));
        assert!(
            surface.maintain(Instant::now()),
            "the hint's going is a frame"
        );
        assert!(surface.stop_armed_at.is_none());
        assert_ne!(surface.footer_line(), Some(hint));
        assert!(press(&mut surface, NamedKey::Escape));
        assert!(
            commands.try_recv().is_err(),
            "a late second press only arms"
        );
        assert_eq!(surface.footer_line(), Some(hint));

        // An open popover takes the press first.
        surface.stop_armed_at = None;
        surface.tool_mode_open = true;
        assert!(press(&mut surface, NamedKey::Escape));
        assert!(!surface.tool_mode_open);
        assert!(surface.stop_armed_at.is_none());

        // With no turn running, Escape is not the stop's.
        surface.attempt = None;
        press(&mut surface, NamedKey::Escape);
        press(&mut surface, NamedKey::Escape);
        assert!(surface.stop_armed_at.is_none());
        assert!(commands.try_recv().is_err());
    }

    /// **Up walks back through what was asked, Down walks forward**, and
    /// past the newest gives back the message being typed. The source is the
    /// chat's own messages and what this surface sent since; equal text in
    /// distinct messages remains two steps.
    #[test]
    fn up_and_down_walk_the_messages_asked_and_give_the_draft_back() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        for markdown in ["first", "second", "second"] {
            let message = user_message(&chat, markdown);
            chat.messages.push(message);
        }
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        type_into(&mut surface, "third");
        assert!(press(&mut surface, NamedKey::Enter));
        assert!(surface.composer.is_blank(), "sent");
        surface.pending_user = None;
        type_into(&mut surface, "draft");

        let walk = |surface: &mut ChatSurface, key: NamedKey| {
            press(surface, key);
            surface.composer.text()
        };
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "third",
            "sent, not read back yet"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowUp), "second");
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "second",
            "a repeated message keeps its own step"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowUp), "first");
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "first",
            "the oldest holds"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "second");
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "second");
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "third");
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowDown),
            "draft",
            "the draft comes back"
        );
        assert_eq!(walk(&mut surface, NamedKey::ArrowDown), "draft");
        assert!(surface.history_cursor.is_none());

        // A message Up brought back has the caret at its start (D100);
        // End takes it to its end.
        walk(&mut surface, NamedKey::ArrowUp);
        type_into(&mut surface, "¡");
        assert_eq!(surface.composer.text(), "¡third");
        press(&mut surface, NamedKey::End);
        type_into(&mut surface, "!");
        assert_eq!(surface.composer.text(), "¡third!");

        // Inside a longer message the arrows move the caret.
        surface.forget_input_walk();
        surface.composer.set_text("line one\nline two");
        assert_eq!(walk(&mut surface, NamedKey::ArrowUp), "line one\nline two");
        assert!(
            surface.history_cursor.is_none(),
            "the caret moved up a line"
        );
        assert_eq!(
            walk(&mut surface, NamedKey::ArrowUp),
            "third",
            "from the first line, Up recalls"
        );

        // The `@` picker's keys come first.
        surface.forget_input_walk();
        surface.composer.clear();
        type_into(&mut surface, "@");
        assert!(surface.composer.picker_open());
        press(&mut surface, NamedKey::ArrowUp);
        assert!(surface.history_cursor.is_none());
        assert_eq!(surface.composer.text(), "@");
    }

    /// The composer as a person sees it: laid out at the window's width,
    /// so a long message wraps.
    fn drawn(surface: &mut ChatSurface) {
        surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
    }

    /// A surface on a chat that has been sent `asked`, in a narrow window.
    fn walked_chat(asked: &[&str]) -> (ChatSurface, std::sync::mpsc::Receiver<Command>) {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        std::mem::forget(_updates);
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 420.0, 700.0));
        let mut chat = transcript();
        for markdown in asked {
            let message = user_message(&chat, markdown);
            chat.messages.push(message);
        }
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        drawn(&mut surface);
        (surface, commands)
    }

    const LONG: [&str; 3] = [
        "The first thing I asked, long enough to wrap over more than one line of the composer",
        "Then a second question, also long enough that it wraps onto a second line at this width",
        "And a third, the newest, which wraps as well because it goes on and on for quite a while",
    ];

    /// **Up walks back through every message, not only the newest** (D100),
    /// long ones included: three Ups reach the first, with a frame drawn
    /// between presses as in the app. Down walks forward again to the
    /// draft; in a message that wraps it first takes the caret to the
    /// last line, as Down does inside any message of several lines.
    #[test]
    fn up_three_times_reaches_the_first_of_three_long_messages() {
        let (mut surface, _commands) = walked_chat(&LONG);
        type_into(&mut surface, "my draft");
        drawn(&mut surface);
        for expected in LONG.iter().rev() {
            assert!(press(&mut surface, NamedKey::ArrowUp));
            drawn(&mut surface);
            assert_eq!(surface.composer.text(), *expected);
            assert!(surface.composer.caret_on_first_line());
            assert!(
                !surface.composer.caret_on_last_line(),
                "it wraps, so a caret at its end would have climbed"
            );
        }
        assert!(press(&mut surface, NamedKey::ArrowUp), "the oldest holds");
        assert_eq!(surface.composer.text(), LONG[0]);
        for expected in [LONG[1], LONG[2], "my draft"] {
            for _ in 0..4 {
                if surface.composer.caret_on_last_line() {
                    break;
                }
                assert!(press(&mut surface, NamedKey::ArrowDown));
                drawn(&mut surface);
            }
            assert!(press(&mut surface, NamedKey::ArrowDown));
            drawn(&mut surface);
            assert_eq!(surface.composer.text(), expected);
            assert!(surface.composer.caret_on_last_line());
        }
        assert!(surface.history_cursor.is_none());
    }

    /// **Inside a message of several lines the arrows move the caret**; only
    /// from its first line does Up step back, and only from its last does
    /// Down step forward.
    #[test]
    fn inside_a_message_of_several_lines_the_arrows_move_the_caret() {
        let (mut surface, _commands) = walked_chat(&["older", "line one\nline two\nline three"]);
        // Up brings the newest back with the caret on its first line…
        press(&mut surface, NamedKey::ArrowUp);
        drawn(&mut surface);
        assert_eq!(surface.composer.text(), "line one\nline two\nline three");
        // …so Down moves into it, line by line, before it steps forward.
        press(&mut surface, NamedKey::ArrowDown);
        drawn(&mut surface);
        assert_eq!(surface.history_cursor, Some(1), "the caret moved down");
        press(&mut surface, NamedKey::ArrowDown);
        drawn(&mut surface);
        assert!(surface.composer.caret_on_last_line());
        press(&mut surface, NamedKey::ArrowUp);
        drawn(&mut surface);
        assert_eq!(surface.history_cursor, Some(1), "and back up");
        press(&mut surface, NamedKey::ArrowUp);
        drawn(&mut surface);
        assert!(surface.composer.caret_on_first_line());
        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(
            surface.composer.text(),
            "older",
            "from the first line, Up steps"
        );
        // Down brings the longer one back with the caret on its last line.
        press(&mut surface, NamedKey::ArrowDown);
        drawn(&mut surface);
        assert_eq!(surface.composer.text(), "line one\nline two\nline three");
        assert!(surface.composer.caret_on_last_line());
        press(&mut surface, NamedKey::ArrowDown);
        assert!(surface.history_cursor.is_none(), "to the (empty) draft");
        assert!(surface.composer.is_blank());
    }

    /// **A recalled message, edited and sent, is a new message**: the edit
    /// is what is sent, the chat's earlier messages are not touched, and the
    /// walk starts again from the newest — the one just sent.
    #[test]
    fn an_edited_recalled_message_is_sent_as_a_new_one() {
        let (mut surface, commands) = walked_chat(&["first", "second"]);
        press(&mut surface, NamedKey::ArrowUp);
        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(surface.composer.text(), "first");
        press(&mut surface, NamedKey::End);
        type_into(&mut surface, " again");
        assert!(press(&mut surface, NamedKey::Enter));
        let sent: Vec<_> = std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                Command::Send(request) => Some(request.markdown),
                _ => None,
            })
            .collect();
        assert_eq!(sent, ["first again"]);
        assert!(surface.history_cursor.is_none(), "the walk starts again");
        let asked: Vec<_> = surface
            .transcript
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .map(|message| message.markdown.as_str())
            .collect();
        assert_eq!(asked, ["first", "second"], "history is not rewritten");
        surface.pending_user = None;
        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(surface.composer.text(), "first again");
        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(surface.composer.text(), "second");
    }

    /// **The draft survives the walk, chip and all** — given back by Down
    /// past the newest, and by Escape while walking.
    #[test]
    fn a_draft_with_a_chip_survives_the_walk_and_escape_gives_it_back() {
        let (mut surface, _commands) = walked_chat(&["first", "second"]);
        type_into(&mut surface, "see ");
        let document = chip_into(&mut surface, "Budget");
        let draft = surface.composer.message();
        assert_eq!(draft.documents, vec![(document, "Budget".to_owned())]);

        press(&mut surface, NamedKey::ArrowUp);
        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(surface.composer.text(), "first");
        press(&mut surface, NamedKey::ArrowDown);
        press(&mut surface, NamedKey::ArrowDown);
        assert_eq!(surface.composer.message(), draft, "Down gives it back");

        press(&mut surface, NamedKey::ArrowUp);
        assert_eq!(surface.composer.text(), "second");
        assert!(press(&mut surface, NamedKey::Escape));
        assert_eq!(surface.composer.message(), draft, "so does Escape");
        assert!(surface.history_cursor.is_none());
        assert!(surface.stop_armed_at.is_none(), "no stop was armed");
    }

    #[test]
    fn sending_a_recalled_message_preserves_the_original_draft_on_success_or_refusal() {
        let (mut surface, commands) = walked_chat(&["older"]);
        surface.composer.set_text("unfinished draft");
        assert!(surface.recall_input(true));
        assert_eq!(surface.composer.text(), "older");
        assert!(surface.send_composer());
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Send(_)
        ));
        assert_eq!(surface.composer.text(), "unfinished draft");
        surface.apply_update(Update::SendFailed(koru_ai_wire::Failure {
            code: "ai_unavailable".into(),
            message: "Try again.".into(),
            retryable: true,
            details: Default::default(),
        }));
        assert_eq!(surface.composer.text(), "older");
        assert!(surface.recall_input(false));
        assert_eq!(surface.composer.text(), "unfinished draft");
    }

    /// **A recalled message keeps its chips**, as the draft does: Up gives
    /// back the document it named as a chip, captioned with the title the
    /// conversation reads it under, so sending it again carries the document.
    #[test]
    fn up_recalls_a_message_with_its_document_chips() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let known = DocumentId::new();
        let unknown = DocumentId::new();
        let sent = format!("Compare [[koru://doc/{known}]] and [[koru://doc/{unknown}]]");
        let mut chat = transcript();
        let message = user_message(&chat, &sent);
        chat.messages.push(message);
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.document_titles.insert(known, "Budget".to_owned());

        assert!(press(&mut surface, NamedKey::ArrowUp));
        let recalled = surface.composer.message();
        assert_eq!(recalled.markdown, sent, "it leaves again as it was sent");
        assert_eq!(
            recalled.documents,
            vec![
                (known, "Budget".to_owned()),
                (
                    unknown,
                    koru_i18n::t(surface.lang, koru_i18n::Key::AiDocumentLinkUntitled).to_owned()
                ),
            ]
        );
    }

    #[test]
    fn attachment_only_history_restages_fresh_ids_and_returns_the_draft() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        let message = user_message(&chat, "");
        let source = koru_ai_core::ChatAttachment {
            id: koru_ai_core::ChatAttachmentId::new(),
            message_id: Some(message.id),
            ordinal: Some(0),
            kind: koru_ai_core::AttachmentKind::File,
            source: koru_ai_core::AttachmentSource::Clipboard,
            file_name: "notes.pdf".into(),
            media_type: "application/pdf".into(),
            byte_count: 12,
            dimensions: None,
            origin: None,
            reference_path: None,
            created_at: koru_ai_core::Timestamp::from_millis(2),
        };
        chat.messages.push(message);
        chat.attachments.push(source.clone());
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.composer.set_text("my draft");

        assert!(surface.recall_input(true));
        let Command::Restage(restage_chat, restage_source, first) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("recall must restage bytes")
        };
        assert_eq!((restage_chat, restage_source), (chat_id, source.id));
        assert_ne!(first, source.id);
        assert_eq!(surface.strip()[0].id, first);
        assert_eq!(surface.composer.text(), "");
        surface.apply_update(Update::Restaged {
            new_id: first,
            result: Ok(koru_ai_core::ChatAttachment {
                id: first,
                ..source.clone()
            }),
        });
        assert_eq!(surface.strip()[0].state, attachments::DraftState::Stored);

        assert!(surface.recall_input(false));
        assert_eq!(surface.composer.text(), "my draft");
        assert!(surface.strip().is_empty());
        assert!(surface.recall_input(true));
        let Command::Restage(_, _, second) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("a second recall needs another staged id")
        };
        assert_ne!(first, second);
    }

    #[test]
    fn document_and_attachment_share_a_bounded_composer_band() {
        let mut surface = ChatSurface::new();
        let mut chat = transcript();
        chat.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        let chat_id = chat.chat.id;
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.set_viewport(Rect::new(0.0, 0.0, 280.0, 650.0));
        let document = DocumentId::new();
        surface.set_context_document(Some((document, "Planning".into())));
        for i in 0..8 {
            surface.strips.push(
                chat_id,
                attachments::Draft::from_path(
                    std::path::PathBuf::from(format!("attachment-{i}.pdf")),
                    koru_ai_core::AttachmentSource::FilePicker,
                ),
            );
        }
        let composer = surface.composer_rect();
        assert_eq!(surface.chip_band.rects.len(), 9);
        assert!(surface.chip_band.max_scroll() > 0.0);
        let chip_h = surface.chip_band.rects[0].h;
        assert_eq!(
            surface.chip_band.shown_height,
            chip_h * 3.0 + composer_band::GAP * 2.0,
        );
        let first = surface
            .chip_band
            .chip_rect(
                0,
                composer.x + COMPOSER_PAD_X,
                composer.y + COMPOSER_PAD_TOP,
            )
            .unwrap();
        let doc_chip = ContextChip {
            label: "Planning",
            secondary: "Koru",
            icon: icons::DOCUMENT,
            state: ContextState::Ready,
        };
        let close = doc_chip.remove_rect(first);
        assert_eq!(
            surface.strip_mouse_down(close.x + close.w * 0.5, close.y + close.h * 0.5),
            Some(true)
        );
        assert!(surface.context_document.is_none());
        assert_eq!(surface.strip().len(), 8);
    }

    #[test]
    fn leaving_a_recall_drops_a_late_restage_completion() {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let mut chat = transcript();
        let message = user_message(&chat, "same");
        let source = koru_ai_core::ChatAttachment {
            id: koru_ai_core::ChatAttachmentId::new(),
            message_id: Some(message.id),
            ordinal: Some(0),
            kind: koru_ai_core::AttachmentKind::File,
            source: koru_ai_core::AttachmentSource::Clipboard,
            file_name: "notes.pdf".into(),
            media_type: "application/pdf".into(),
            byte_count: 12,
            dimensions: None,
            origin: None,
            reference_path: None,
            created_at: koru_ai_core::Timestamp::from_millis(2),
        };
        chat.messages.push(message);
        chat.attachments.push(source.clone());
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        assert!(surface.recall_input(true));
        let Command::Restage(_, _, new_id) =
            commands.recv_timeout(Duration::from_millis(50)).unwrap()
        else {
            panic!("restage")
        };
        assert!(surface.recall_input(false));
        assert!(surface.strip().is_empty());
        surface.apply_update(Update::Restaged {
            new_id,
            result: Ok(source),
        });
        assert!(surface.strip().is_empty());
        assert!(!surface.recall_retired.contains_key(&new_id));
    }

    /// **An empty chat greets the person**, with one line picked when the
    /// chat starts and held while it shows; a message ends it, and another
    /// chat picks again.
    #[test]
    fn an_empty_chat_greets_once_and_holds_the_line() {
        let mut surface = ChatSurface::new();
        surface.validated_ready = true;
        let line = surface.greeting().expect("a chat being made greets");
        for _ in 0..4 {
            surface.draw_chat(&mut Scene::new(), Affine::IDENTITY);
            assert_eq!(surface.greeting(), Some(line), "the line holds still");
        }
        surface.transcript = Some(transcript());
        assert_eq!(
            surface.greeting(),
            Some(line),
            "the chat it waited for keeps it"
        );

        surface.pending_user = Some("hello".into());
        assert_eq!(surface.greeting(), None, "a message on its way ends it");
        surface.pending_user = None;
        let message = user_message(surface.transcript.as_ref().unwrap(), "hello");
        surface.transcript.as_mut().unwrap().messages.push(message);
        assert_eq!(surface.greeting(), None);

        let other = transcript();
        let other_id = other.chat.id;
        surface.transcript = Some(other);
        assert!(surface.greeting().is_some());
        assert_eq!(surface.greeting.map(|(chat, _)| chat), Some(Some(other_id)));

        let mut loading = ChatSurface::new();
        loading.open_link(ChatId::new(), None);
        assert_eq!(
            loading.greeting(),
            None,
            "a chat still loading is not empty"
        );
    }

    #[test]
    fn the_empty_chat_greeting_draws_the_title_sized_koru_mark() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        surface.composer_rect();
        let capture = koru_draw::inspect::Frame::begin(
            1,
            Affine::IDENTITY,
            scene::rect(0.0, 0.0, 900.0, 700.0),
        );
        surface.draw_greeting(&mut Scene::new(), Affine::IDENTITY);
        let snapshot = capture.finish();
        let logo = snapshot
            .records
            .iter()
            .find(|record| record.name == "KoruLogo")
            .unwrap();
        let rect = logo.rect();
        assert_eq!(rect.width() as f32, role::title().metrics.size);
        assert!(rect.y1 <= surface.composer_hit_rect.y as f64);
    }

    #[test]
    fn the_plus_menu_opens_below_an_empty_chat_and_above_a_begun_one() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 900.0));
        surface.transcript = Some(transcript());
        surface.composer_rect();
        surface.open_plus_menu();
        let empty_panel = surface
            .plus_menu
            .as_ref()
            .unwrap()
            .panel(surface.menu_viewport());
        assert!(empty_panel.y >= surface.composer_hit_rect.y + surface.composer_hit_rect.h);

        surface.pending_user = Some("Plan the launch".into());
        surface.composer_rect();
        surface.open_plus_menu();
        let begun_panel = surface
            .plus_menu
            .as_ref()
            .unwrap()
            .panel(surface.menu_viewport());
        assert!(begun_panel.y + begun_panel.h <= surface.composer_hit_rect.y);
    }

    /// **The composer starts in the middle, then moves to the bottom.** While
    /// the chat is empty the greeting and the composer are one centred block;
    /// once a message is on its way the composer sits at the bottom. The send
    /// button, the toolbar, the text and the conversation follow it, inside
    /// the composer's padding, in both places.
    #[test]
    fn the_composer_starts_centred_and_moves_to_the_bottom() {
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        surface.transcript = Some(transcript());
        surface.validated_ready = true;

        let centred = surface.composer_rect();
        let bottom = surface.viewport.h - COMPOSER_BOTTOM_MARGIN - centred.h;
        let above = role::title().metrics.line_height + GREETING_GAP + COMPOSER_GAP;
        let middle = (centred.y - above + centred.y + centred.h) * 0.5;
        assert!((middle - surface.viewport.h * 0.5).abs() < 1.0);
        assert!(centred.y < bottom - 100.0);
        for begun in [false, true] {
            surface.pending_user = begun.then(|| "hello".to_owned());
            let composer = surface.composer_rect();
            let send = surface.send_button();
            let toolbar = surface.composer_toolbar_rect();
            let text = surface.composer_text_rect();
            assert_eq!(text.y, composer.y);
            assert_eq!(toolbar.x, composer.x + COMPOSER_PAD_X);
            assert_eq!(send.x + send.w, composer.x + composer.w - COMPOSER_PAD_X);
            assert_eq!(
                toolbar.y + toolbar.h,
                composer.y + composer.h - COMPOSER_PAD_BOTTOM
            );
            assert_eq!(
                send.y + send.h,
                composer.y + composer.h - COMPOSER_PAD_BOTTOM
            );
            assert!(text.y + text.h <= toolbar.y);
            let conversation = surface.conversation_rect();
            assert!(conversation.y + conversation.h < composer.y);
        }
        assert_eq!(surface.composer_rect().y, bottom, "at the bottom");

        // Pressed where it is drawn: the text in the middle takes focus.
        surface.pending_user = None;
        let text = {
            surface.composer_rect();
            surface.composer_text_rect()
        };
        surface.focus = Focus::None;
        assert!(surface.mouse_down(text.x + 40.0, text.y + text.h * 0.5));
        assert_eq!(surface.focus, Focus::Composer);
    }

    /// **The composer is one component in the inspector**, its box, text,
    /// toolbar parts under it, and every primitive it paints
    /// — the box's fill and stroke among them — inside one of those.
    #[test]
    fn the_composer_is_one_component_with_its_parts_inside() {
        use koru_draw::inspect;
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
        let mut transcript = transcript();
        transcript.chat.current_model.provider = AiProviderId::parse("agent-codex").unwrap();
        transcript.chat.current_model.requested_model = AGENT_DEFAULT_MODEL.into();
        transcript.messages.clear();
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.setup_open = false;
        let root = Affine::IDENTITY;
        let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 900.0, 600.0));
        surface.draw(&mut Scene::new(), root);
        let records = capture.finish().records;
        let composer = records
            .iter()
            .position(|r| r.name == "KoruComposer")
            .expect("the composer is a component");
        let under = |mut i: usize| {
            while let Some(parent) = records[i].parent {
                if parent == composer {
                    return true;
                }
                i = parent;
            }
            false
        };
        let the_box = records
            .iter()
            .position(|r| r.name == "ComposerBox")
            .expect("the box is drawn");
        assert_eq!(records[the_box].parent, Some(composer));
        for part in ["ComposerText", "ComposerToolbar"] {
            let i = records
                .iter()
                .position(|r| r.name == part)
                .unwrap_or_else(|| panic!("{part} is drawn"));
            assert_eq!(
                records[i].parent,
                Some(the_box),
                "{part} is inside the composer's box"
            );
        }
        let primitives: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.parent == Some(the_box))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !primitives.is_empty(),
            "the box's fill and stroke are inside it"
        );
        assert!(primitives.into_iter().all(under));
    }

    #[test]
    fn painted_model_stays_left_of_the_microphone() {
        use koru_draw::inspect;
        let mut surface = ChatSurface::new();
        surface.set_viewport(Rect::new(0.0, 0.0, 560.0, 600.0));
        let mut chat = transcript();
        chat.chat.current_model.requested_model = "a-long-model-name-for-layout".into();
        surface.transcript = Some(chat);
        surface.validated_ready = true;
        surface.offer_dictation(true);
        surface.offer_microphone_choice(true);
        let capture =
            inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 560.0, 600.0));
        surface.draw(&mut Scene::new(), Affine::IDENTITY);
        let records = capture.finish().records;
        let microphone = records
            .iter()
            .find(|record| record.name == "ComposerMicrophone")
            .expect("microphone in the composer frame")
            .rect();
        let (model_index, model_record) = records
            .iter()
            .enumerate()
            .filter(|record| {
                record.1.name == "KoruButton"
                    && record.1.rect().y0 >= microphone.y0
                    && record.1.rect().y1 <= microphone.y1
            })
            .max_by(|a, b| a.1.rect().x1.total_cmp(&b.1.rect().x1))
            .expect("model control painted in the toolbar");
        let model = model_record.rect();
        let label = records
            .iter()
            .find(|record| record.name == "Button label" && record.parent == Some(model_index))
            .expect("model label painted inside the control")
            .rect();
        assert!(
            model.x1 <= microphone.x0,
            "model control overlaps microphone"
        );
        assert!(label.x1 <= microphone.x0, "model text overlaps microphone");
    }

    /// **What a chat is doing, for the marks outside it** (D83): working
    /// from the send until the stream ends, needs you while a call waits
    /// for an approval, and one arrival per reply that ends — however many
    /// times the host asks.
    #[test]
    fn a_chat_says_it_works_waits_and_finished_once() {
        let mut surface = ChatSurface::new();
        let (reader, _commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let transcript = transcript();
        surface.transcript = Some(transcript.clone());
        assert!(!surface.is_working());
        assert_eq!(surface.chat_status(false), koru_chats_ui::ChatStatus::Idle);
        assert_eq!(surface.chat_status(true), koru_chats_ui::ChatStatus::Unread);
        assert!(!surface.take_arrival());

        let attempt_id = InferenceAttemptId::new();
        surface.pending_user = Some("Plan the launch".into());
        surface.apply_update(Update::Accepted(koru_ai_wire::SendTurnAccepted {
            attempt_id,
            user_message_id: MessageId::new(),
            assistant_message_id: MessageId::new(),
            replayed: false,
        }));
        assert!(surface.is_working());
        assert_eq!(
            surface.chat_status(true),
            koru_chats_ui::ChatStatus::Working
        );

        surface.upsert_tool_activity(ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id,
            chat_id: transcript.chat.id,
            assistant_message_id: MessageId::new(),
            tool_name: "edit_document".into(),
            read_only: false,
            status: ToolActivityStatus::ApprovalRequired,
            approval: ToolApproval::Pending,
            proposed_at: koru_ai_core::Timestamp::from_millis(2),
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            text_offset: None,
            display_target: None,
            live: None,
        });
        assert!(surface.needs_you());
        assert_eq!(
            surface.chat_status(false),
            koru_chats_ui::ChatStatus::NeedsYou
        );
        assert!(surface.take_arrival(), "a question arrived");
        assert!(!surface.take_arrival(), "once");

        surface.apply_update(Update::Event(koru_ai_wire::StreamEvent {
            version: koru_ai_wire::AI_STREAM_VERSION,
            sequence: 1,
            event: PublicAiEvent::Completed {
                attempt_id,
                stop_reason: None,
                at: koru_ai_core::Timestamp::from_millis(3),
            },
        }));
        assert!(!surface.is_working(), "the stream ended");
        assert_eq!(surface.finished_replies(), 1);
        assert!(surface.take_arrival(), "a reply ended");
        assert!(!surface.take_arrival(), "once");
    }

    /// What a look has seen is the newest message that is done: a reply
    /// still streaming is not it.
    #[test]
    fn the_newest_seen_message_skips_a_reply_still_running() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let message = |ordinal, state| koru_ai_core::Message {
            id: MessageId::new(),
            chat_id: transcript.chat.id,
            ordinal,
            role: ChatRole::User,
            state,
            markdown: "Hi".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: None,
        };
        let (first, running) = (
            message(0, MessageState::Complete),
            message(1, MessageState::Pending),
        );
        transcript.messages = vec![first.clone(), running];
        surface.transcript = Some(transcript);
        assert_eq!(surface.newest_seen_message(), Some(first.id));
    }

    /// **An unread chat opens on its NEW divider** (D84): the band goes in
    /// front of the first message after the one seen last, the chat opens
    /// there rather than at its end — once — and a look with nothing new
    /// puts the divider away.
    #[test]
    fn an_unread_chat_opens_on_its_divider_above_the_first_unseen_message() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        for ordinal in 0..24 {
            transcript.messages.push(koru_ai_core::Message {
                id: MessageId::new(),
                chat_id: transcript.chat.id,
                ordinal,
                role: if ordinal % 2 == 0 {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                state: MessageState::Complete,
                markdown: format!("message {ordinal}"),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1),
                completed_at: Some(koru_ai_core::Timestamp::from_millis(ordinal as i64 + 1)),
            });
        }
        let chat = transcript.chat.id;
        let seen = transcript.messages[11].id;
        let first_new = transcript.messages[12].id;
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.scroll_loaded_chat_to_bottom = true;
        assert!(surface.show_new_from(chat, Some(seen)));

        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);

        let index = surface.new_divider_index().expect("a message came after");
        assert_eq!(surface.message_layouts[index].id, Some(first_new));
        let top = surface
            .new_divider
            .as_ref()
            .and_then(|divider| divider.top)
            .expect("placed");
        assert!(top > surface.message_layouts[index - 1].top);
        assert!(
            top + new_divider::NEW_DIVIDER_H <= surface.message_layouts[index].top,
            "a band of its own, covering no message"
        );
        let bottom = (surface.conversation_extent - surface.conversation_rect().h).max(0.0);
        assert!(
            surface.conversation_scroll < bottom,
            "opened on the divider, not at the end"
        );
        assert!(surface.conversation_scroll <= top);
        assert!(!surface.scroll_loaded_chat_to_bottom);

        // Once: a later layout leaves the reader where they scrolled.
        surface.conversation_scroll = 0.0;
        surface.messages_dirty = true;
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        assert_eq!(surface.conversation_scroll, 0.0);

        // Nothing new the next time: the divider goes, and its band with it.
        assert!(surface.show_new_from(chat, None));
        assert!(!surface.show_new_from(chat, None), "already gone");
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        assert!(surface.new_divider_index().is_none());
    }

    /// A divider over the last message has nothing to go above.
    #[test]
    fn a_divider_after_the_newest_message_shows_nothing() {
        let mut surface = ChatSurface::new();
        let mut transcript = transcript();
        let only = user_message(&transcript, "Hello");
        transcript.messages.push(only.clone());
        let chat = transcript.chat.id;
        surface.transcript = Some(transcript);
        surface.viewport = Rect::new(0.0, 0.0, 480.0, 360.0);
        surface.show_new_from(chat, Some(only.id));
        surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
        assert!(surface.new_divider_index().is_none());
        assert!(surface.new_divider.as_ref().unwrap().top.is_none());
    }

    /// **Ctrl+Shift+↑/↓ step through the Chats list** (D87): the host, whose
    /// list it is, is asked — the surface reads nothing in place; with text
    /// in the composer the chord is the editor's and nothing is asked.
    #[test]
    fn ctrl_shift_arrows_step_through_the_chats_list_with_an_empty_composer() {
        let primary_shift = if cfg!(target_os = "macos") {
            ModifiersState::SUPER | ModifiersState::SHIFT
        } else {
            ModifiersState::CONTROL | ModifiersState::SHIFT
        };
        let down = Key::Named(NamedKey::ArrowDown);
        let up = Key::Named(NamedKey::ArrowUp);

        let mut pane = ChatSurface::new();
        pane.transcript = Some(transcript());
        pane.validated_ready = true;
        pane.setup_open = false;
        assert!(pane.key(&down, None, primary_shift));
        assert_eq!(
            pane.take_wants_chat_step(),
            Some(koru_chats_ui::ChatStep::Next)
        );
        pane.composer.set_text("two\nlines");
        pane.key(&up, None, primary_shift);
        assert_eq!(pane.take_wants_chat_step(), None, "the editor's chord");

        let mut reading = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        reading.reader = Some(reader);
        reading.validated_ready = true;
        reading.setup_open = false;
        reading.transcript = Some(transcript());
        assert!(reading.key(&up, None, primary_shift));
        assert_eq!(
            reading.take_wants_chat_step(),
            Some(koru_chats_ui::ChatStep::Previous)
        );
        assert!(commands.try_recv().is_err(), "nothing read in place");
        assert_eq!(
            koru_keys::registry::get(koru_keys::ShortcutId::NextChat).layer,
            koru_keys::Layer::Chat
        );
    }

    /// Reduced motion holds the light over *Working for …* still: the work
    /// view ticks once a second for its count, not at the status frame.
    #[test]
    fn reduced_motion_ticks_the_work_header_once_a_second() {
        let mut surface = ChatSurface::new();
        assert!(surface.work_tick_deadline().is_none(), "no reply runs");
        surface.work_layouts.push(work_view::build(
            &mut surface.engine,
            koru_i18n::Lang::En,
            300.0,
            work_view::WorkInput {
                message: MessageId::new(),
                live: true,
                started_ms: 0,
                worked_ms: 0,
                items: Vec::new(),
                groups: Vec::new(),
            },
            &work_view::Opened {
                replies: &HashSet::new(),
                groups: &HashSet::new(),
                calls: &HashSet::new(),
            },
            &mut |_, _, _| Rc::new(Vec::new()),
        ));
        let now = Instant::now();
        surface.work_ticked_at = Some(now);
        assert_eq!(
            surface.work_tick_deadline(),
            Some(now + koru_chats_ui::status::FRAME)
        );
        surface.set_reduced_motion(true);
        assert_eq!(
            surface.work_tick_deadline(),
            Some(now + Duration::from_secs(1))
        );
    }
}
