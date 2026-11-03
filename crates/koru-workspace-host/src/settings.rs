// SPDX-License-Identifier: MPL-2.0

//! The session: what the app has to remember so it reopens where you left it.
//!
//! Window geometry, the two side columns, and the tabs that were open.
//!
//! **The preference fields here are a view, not a store.** `theme`, the fonts,
//! the caret, the dictation choices and the rest are read from the settings
//! authority and patched back to it through [`crate::preferences`]; this module
//! opens no file for any of them. It used to own the largest of
//! `preferences.conf`'s three parsers and its only writer — see ADR-266 for why
//! that stopped being tenable, and `migrate_relocated_keys` below for the one
//! read of that file left in the crate.
//!
//! Written as `key=value` lines rather than JSON so the prototype needs no
//! serialization dependency, and so the file stays hand-editable. **Anything
//! unparseable is ignored rather than fatal**, and that is load-bearing now
//! that a line can hold a list: one bad tab entry drops itself and leaves its
//! neighbours alone, a missing key takes its default, and an older file with
//! none of the new keys still opens. The worst case is one clean first run,
//! never a failure to start.

use koru_settings::FontSetting;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_draw::theme::{CaretColor, CaretHeight, CaretWidth, Density, ThemePreference, ViewMode};
use koru_session_protocol::{
    BuiltInProfileKey, ProjectId, SessionId, SessionProfileId, SessionProfileKey,
};
use koru_voice::{InputChoice, Requested};
use koru_voice_models::ModelTier;

/// A tab, in a form that survives the process.
///
/// Not a `NodeId`: the welcome screen is not a node, and a list of ids could
/// not say which slot it was in.
///
/// There used to be a third kind, `workspace` — the permanent outline tab.
/// **`parse` deliberately does not know that word any more**, so a settings
/// file written by an older build drops that entry and keeps every other tab,
/// which is exactly the tolerance `from_text` already promises for a line it
/// cannot read. Nothing has to migrate, and nothing writes it again.
// Not `Copy` any more: [`Session`](Self::Session) carries a
// `SessionProfileKey`, whose custom arm owns a string. Consumers clone, which
// a handful of tabs on a save path can well afford.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRef {
    /// The welcome screen — recents, and the ways in.
    Welcome,
    /// A node, opened by whatever view its kind calls for.
    ///
    /// **The folder world's spelling.** A document was a node in the library
    /// tree then, so remembering the node remembered the tab. Store-backed a
    /// document is its own document, and [`Document`](Self::Document) is what a
    /// tab records; this stays because an existing settings file is full of
    /// them and reopening on a welcome tab is a worse answer than reopening on
    /// nothing.
    Node(NodeId),
    /// A document in a workspace's manifest.
    ///
    /// Written with a `doc:` prefix so the two cannot be confused on the way
    /// back in: both are v4 uuids, and a `NodeId` read as a `DocumentId` would
    /// name a document that does not exist and quietly shorten the list.
    ///
    /// `workspace` is the document's home. The open strip is window-scoped, so a
    /// tab from another workspace must still name where to reopen it. Legacy
    /// lines without `@workspace` parse as `None` and pick up the window's
    /// current workspace on restore.
    Document {
        id: koru_core::DocumentId,
        workspace: Option<WorkspaceId>,
    },
    /// A blank page whose document has not been created yet.
    ///
    /// It names nothing, and that is the point: a draft *is* a blank page, so
    /// restoring one is making a new blank page. Stored as its own token rather
    /// than folded into `Welcome` so that reopening gives back the tab that was
    /// there — somebody who left an empty page open meant to come back to it.
    Draft,
    /// The workspace manager. Names no node, because it is about the *list* of
    /// workspaces rather than about anything inside one.
    Workspaces,
    /// The Profile and account management tab.
    ManageProfiles,
    /// The Manage tab — Profiles, New profile, linked devices and Koru Cloud.
    /// Spelled `manage`; it restores on the open Profile's page, since the
    /// page is where a person was, not what the tab is. An **older reader**
    /// knows no such word and drops the entry, keeping every other tab.
    Manage,
    /// The Trash tab.
    Trash,
    /// The Tasks tab, and **which queue it was on**.
    ///
    /// The bucket or list is part of the tab's identity here rather than a
    /// preference beside it: reopening on the wrong queue is reopening a
    /// different page, and the rail is put back from this so the column and the
    /// content cannot disagree on the first frame.
    ///
    /// Written as `tasks:<token>` — one of the five agenda words, or
    /// `list:<uuid>`. A token this build cannot read drops the entry and keeps
    /// every other tab, which is the tolerance `from_text` already promises.
    Tasks(TasksTabRef),
    /// This build's release notes.
    ///
    /// **Carries no version**, deliberately: the notes are re-read from beside
    /// the executable on restore, so a session captured before an update comes
    /// back showing the *newer* build's notes. That is the honest answer — the
    /// tab is "what this build changed", and after an update this build is a
    /// different one.
    WhatsNew,
    /// The Sync settings page. Spelled `sync-settings`; an **older reader**
    /// knows no such word and drops the entry, keeping every other tab.
    SyncSettings,
    /// A running shell or agent session, hosted as an ordinary tab
    /// (TERMINATAB W4).
    ///
    /// `session` is the host's stable id for the process this tab was over;
    /// restore reattaches it while the Agent still reports it, and presents
    /// the honest session-lost state when it does not. `profile` is the stable
    /// [`SessionProfileKey`] — identity, never a translated label — which is
    /// what labels the lost state and what *resume* launches afresh; a dead
    /// session id is never revived.
    ///
    /// Written as `session:<uuid>@<profile>` (built-in keys spell themselves;
    /// a custom profile is `custom:<uuid>`), so an **older reader** — whose
    /// grammar knows keywords, `doc:` and bare node ids — parses nothing here
    /// and drops the entry on its own, keeping every neighbouring tab. That is
    /// the same tolerance the retired `workspace` token already relies on.
    ///
    /// **`recovery` is what the tab keeps for the day its session is gone**
    /// (ADR-474): the folder it was in, the project it was placed in and
    /// whether it was a raw terminal. It is written as a `|`-separated suffix
    /// of percent-encoded `key=value` pairs, so neither the list's comma nor
    /// the scroll's colon can appear in it, and an older reader — which
    /// cannot read the profile token through the suffix — drops the entry, as
    /// it already does for a session entry it does not understand.
    Session {
        session: SessionId,
        profile: SessionProfileKey,
        recovery: Option<SessionRecovery>,
    },
    /// An AI chat tab, by the chat it is over (ADR-483-chattabrestore).
    ///
    /// The id is the AI service's `ChatId`, held as its uuid because this
    /// crate does not depend on the AI crates. Only a chat the service holds
    /// is ever written — a new chat not sent yet has nothing to reopen.
    ///
    /// Written as `chat:<uuid>`. An **older reader** matches none of its
    /// words, prefixes or a bare node id (`chat:<uuid>` is not a uuid), so it
    /// drops this one entry and keeps every other tab.
    Chat(uuid::Uuid),
}

/// One agent's terminal look: each half is `None` when that half follows the
/// Settings page. Tokens, not enums — `light`/`dark` and a monospaced font
/// family's token — so this crate holds no opinion on what exists, and a token
/// a later build adds or removes reads as "follow Settings" rather than
/// failing the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalLook {
    pub theme: Option<String>,
    pub font: Option<String>,
}

impl TerminalLook {
    pub fn is_default(&self) -> bool {
        self.theme.is_none() && self.font.is_none()
    }
}

/// `claude|dark|jetbrains-mono;codex||fira-code`: entries by `;`, fields by
/// `|`, an empty field meaning *follow Settings*. Characters that would be
/// grammar are dropped from a token on the way out, so a hand-edited value can
/// never break the line.
fn write_looks(looks: &std::collections::BTreeMap<String, TerminalLook>) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .filter(|c| !matches!(c, ';' | '|' | '\n' | '\r' | '='))
            .collect()
    };
    looks
        .iter()
        .filter(|(_, look)| !look.is_default())
        .map(|(agent, look)| {
            format!(
                "{}|{}|{}",
                clean(agent),
                look.theme.as_deref().map(clean).unwrap_or_default(),
                look.font.as_deref().map(clean).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn read_looks(value: &str) -> std::collections::BTreeMap<String, TerminalLook> {
    value
        .split(';')
        .filter_map(|entry| {
            let mut fields = entry.split('|');
            let agent = fields.next()?.trim();
            let field = |text: Option<&str>| {
                text.map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
            };
            let look = TerminalLook {
                theme: field(fields.next()),
                font: field(fields.next()),
            };
            (!agent.is_empty() && !look.is_default()).then(|| (agent.to_owned(), look))
        })
        .collect()
}

/// What a saved session tab needs to start a replacement in the same place.
///
/// Every field is optional on the way back in: a hand-edited or older entry
/// that lacks one simply offers a fresh session from the default folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionRecovery {
    /// The folder the session's process started in, as the host reported it.
    pub working_directory: Option<String>,
    /// The project the session was placed in, if any.
    pub project: Option<ProjectId>,
    /// A raw terminal rather than a managed chat. A recovered raw agent may
    /// *resume*; a recovered managed chat has no conversation to continue.
    pub raw: bool,
}

impl SessionRecovery {
    fn write(&self) -> String {
        let mut out = String::new();
        if self.raw {
            out.push_str("|raw=1");
        }
        if let Some(dir) = &self.working_directory {
            out.push_str("|dir=");
            out.push_str(&percent_encode(dir));
        }
        if let Some(project) = self.project {
            out.push_str("|project=");
            out.push_str(&project.to_string());
        }
        out
    }

    fn read<'a>(pairs: impl Iterator<Item = &'a str>) -> Option<Self> {
        use std::str::FromStr as _;
        let mut recovery = Self::default();
        let mut any = false;
        for pair in pairs {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "raw" => recovery.raw = value == "1",
                "dir" => recovery.working_directory = percent_decode(value),
                "project" => recovery.project = ProjectId::from_str(value).ok(),
                // A key a later build added: read past it, keep the rest.
                _ => continue,
            }
            any = true;
        }
        any.then_some(recovery)
    }
}

/// Everything but letters, digits and `-_.` becomes `%XX`, so a path's colon,
/// backslash, comma or `|` can never be read as grammar.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out)
        .ok()
        .filter(|decoded| !decoded.is_empty())
}

/// Which page a Tasks tab was on — a queue, a list, or the Reminders page.
///
/// **The bucket is a token, not a third enum.** `koru-core` owns the `Bucket`
/// the app's rail shows and `koru-store-wire` owns the five words it is spelled
/// with; this crate has no business declaring a fourth spelling, so it
/// carries one of [`BUCKET_TOKENS`] — and a word this build does not know never
/// becomes a `String` it would then have to hand back out.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TasksTabRef {
    Bucket(&'static str),
    List(koru_core::ids::ListId),
    /// The rail's Reminders row — the All Reminders page hosted as a tab.
    ///
    /// Spelled `tasks:reminders`. An **older reader** knows neither the
    /// `list:` prefix nor a bucket by that name, so [`parse`](Self::parse)
    /// answers `None` there and the entry — and only the entry — is dropped,
    /// the same tolerance an unknown bucket token already got.
    Reminders,
}

/// The five agenda words, in agenda order. Held equal to
/// `koru_store_wire::tasks::BUCKETS` by the caller that maps between them —
/// this crate cannot see that one without acquiring the store's wire types for
/// a settings line.
pub const BUCKET_TOKENS: [&str; 5] = ["today", "next", "upcoming", "later", "completed"];

impl TasksTabRef {
    fn to_storage(self) -> String {
        match self {
            Self::Bucket(token) => format!("tasks:{token}"),
            Self::List(id) => format!("tasks:list:{}", id.to_storage()),
            Self::Reminders => "tasks:reminders".to_string(),
        }
    }

    fn parse(rest: &str) -> Option<Self> {
        if let Some(id) = rest.strip_prefix("list:") {
            return koru_core::ids::ListId::parse(id).map(Self::List);
        }
        if rest == "reminders" {
            return Some(Self::Reminders);
        }
        BUCKET_TOKENS
            .into_iter()
            .find(|token| *token == rest)
            .map(Self::Bucket)
    }
}

impl TabRef {
    fn to_storage(&self) -> String {
        match self {
            Self::Welcome => "welcome".to_string(),
            Self::Workspaces => "workspaces".to_string(),
            Self::ManageProfiles => "profiles".to_string(),
            Self::Manage => "manage".to_string(),
            Self::Trash => "trash".to_string(),
            Self::Tasks(target) => target.to_storage(),
            Self::WhatsNew => "whatsnew".to_string(),
            Self::SyncSettings => "sync-settings".to_string(),
            Self::Draft => "draft".to_string(),
            Self::Node(id) => id.to_storage(),
            Self::Document {
                id,
                workspace: Some(ws),
            } => {
                format!("doc:{}@{}", id.to_storage(), ws.to_storage())
            }
            Self::Document {
                id,
                workspace: None,
            } => format!("doc:{}", id.to_storage()),
            // `{session}` and the ids inside `{profile}` render as canonical
            // hyphenated uuids and `{key}` as a lowercase token — none of which
            // can hold the grammar's comma, and whose trailing segment can
            // never read as the `:<scroll>` suffix `parse_tabs` peels (a uuid
            // always carries hyphens; a token starts with a letter and the
            // built-in list holds no float spelling).
            Self::Session {
                session,
                profile,
                recovery,
            } => {
                let suffix = recovery
                    .as_ref()
                    .map(SessionRecovery::write)
                    .unwrap_or_default();
                match profile {
                    SessionProfileKey::BuiltIn(key) => format!("session:{session}@{key}{suffix}"),
                    SessionProfileKey::Custom(id) => {
                        format!("session:{session}@custom:{id}{suffix}")
                    }
                }
            }
            // Hyphenated, so its last segment can never read as the scroll.
            Self::Chat(chat) => format!("chat:{}", chat.hyphenated()),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "welcome" => Some(Self::Welcome),
            "workspaces" => Some(Self::Workspaces),
            "profiles" => Some(Self::ManageProfiles),
            "manage" => Some(Self::Manage),
            "trash" => Some(Self::Trash),
            "whatsnew" => Some(Self::WhatsNew),
            "sync-settings" => Some(Self::SyncSettings),
            "draft" => Some(Self::Draft),
            other if other.starts_with("tasks:") => {
                TasksTabRef::parse(&other["tasks:".len()..]).map(Self::Tasks)
            }
            other if other.starts_with("chat:") => uuid::Uuid::try_parse(&other["chat:".len()..])
                .ok()
                .map(Self::Chat),
            other => match other.strip_prefix("session:") {
                Some(rest) => Self::parse_session(rest),
                None => match other.strip_prefix("doc:") {
                    Some(rest) => {
                        let (id, workspace) = match rest.split_once('@') {
                            Some((id, ws)) => (id, WorkspaceId::parse(ws)),
                            None => (rest, None),
                        };
                        koru_core::DocumentId::parse(id).map(|id| Self::Document { id, workspace })
                    }
                    None => NodeId::parse(other).map(Self::Node),
                },
            },
        }
    }

    /// The body of a `session:` entry: `<session uuid>@<profile>`.
    ///
    /// Strict on both halves — a session entry that cannot name its session
    /// *and* its profile is not a tab restore can act on, so it drops itself
    /// the way any unreadable entry does rather than coming back half-blank.
    fn parse_session(rest: &str) -> Option<Self> {
        use std::str::FromStr as _;
        // `<session uuid>@<profile>` then, optionally, `|key=value` pairs.
        let mut parts = rest.split('|');
        let head = parts.next()?;
        let recovery = SessionRecovery::read(parts);
        let (session, profile) = head.split_once('@')?;
        let session = SessionId::from_str(session).ok()?;
        let profile = match profile.strip_prefix("custom:") {
            Some(id) => SessionProfileKey::Custom(SessionProfileId::from_str(id).ok()?),
            None => SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str(profile).ok()?),
        };
        Some(Self::Session {
            session,
            profile,
            recovery,
        })
    }

    /// Document id when this ref names one.
    pub fn document_id(&self) -> Option<koru_core::DocumentId> {
        match self {
            Self::Document { id, .. } => Some(*id),
            _ => None,
        }
    }

    /// A document tab in the given home workspace.
    pub fn document(id: koru_core::DocumentId, workspace: Option<WorkspaceId>) -> Self {
        Self::Document { id, workspace }
    }
}

/// What a pinned tab is over: a document, or an AI chat (ADR-483-chattabrestore).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PinnedTarget {
    Document(DocumentId),
    /// The AI service's `ChatId`, as [`TabRef::Chat`] carries it. Written
    /// `chat:` where a document is `doc:`, which an older reader's pinned
    /// grammar requires, so it drops this one pin and keeps the rest.
    Chat(uuid::Uuid),
}

/// A pinned tab, window-scoped and independent of the current workspace.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PinnedTabRef {
    pub target: PinnedTarget,
    /// The workspace it was pinned in, and a document's home.
    pub workspace: WorkspaceId,
    pub scroll: f32,
    pub show_title: bool,
}

impl PinnedTabRef {
    /// A pinned document tab.
    pub fn document_pin(
        document: DocumentId,
        workspace: WorkspaceId,
        scroll: f32,
        show_title: bool,
    ) -> Self {
        Self {
            target: PinnedTarget::Document(document),
            workspace,
            scroll,
            show_title,
        }
    }

    /// The document this pin is over; `None` for a chat.
    pub fn document(&self) -> Option<DocumentId> {
        match self.target {
            PinnedTarget::Document(id) => Some(id),
            PinnedTarget::Chat(_) => None,
        }
    }

    fn to_storage(self) -> String {
        let target = match self.target {
            PinnedTarget::Document(id) => format!("doc:{}", id.to_storage()),
            PinnedTarget::Chat(chat) => format!("chat:{}", chat.hyphenated()),
        };
        format!(
            "{target}@{}:{:.0}:{}",
            self.workspace.to_storage(),
            self.scroll,
            if self.show_title { "title" } else { "icon" }
        )
    }

    fn parse(s: &str) -> Option<Self> {
        let (rest, title) = match s.rsplit_once(':') {
            Some((rest, "title")) => (rest, true),
            Some((rest, "icon")) => (rest, false),
            _ => (s, false),
        };
        let (rest, scroll) = match rest.rsplit_once(':') {
            Some((w, sc)) => match sc.parse::<f32>() {
                Ok(v) if v.is_finite() => (w, v.max(0.0)),
                _ => (rest, 0.0),
            },
            None => (rest, 0.0),
        };
        let (target, workspace) = rest.split_once('@')?;
        let target = match (target.strip_prefix("doc:"), target.strip_prefix("chat:")) {
            (Some(id), _) => PinnedTarget::Document(DocumentId::parse(id)?),
            (_, Some(id)) => PinnedTarget::Chat(uuid::Uuid::try_parse(id).ok()?),
            _ => return None,
        };
        let workspace = WorkspaceId::parse(workspace)?;
        Some(Self {
            target,
            workspace,
            scroll,
            show_title: title,
        })
    }
}

/// What closing a popped-out document window does, once the user has asked to
/// remember their choice.
///
/// The default is [`PopoutClose::Ask`]: nothing is remembered, and the close
/// dialog is shown every time. The two remembered values are the two choices
/// that dialog offers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PopoutClose {
    /// Show the close dialog each time (the default).
    Ask,
    /// Close the window and leave the document closed everywhere.
    Close,
    /// Close the window and restore the document as a tab in the container.
    RestoreTab,
}

impl Default for PopoutClose {
    fn default() -> Self {
        Self::Ask
    }
}

impl PopoutClose {
    pub(crate) fn to_storage(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Close => "close",
            Self::RestoreTab => "restore",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "close" => Self::Close,
            "restore" => Self::RestoreTab,
            // An unknown value (a typo, a future spelling) is Ask rather than a
            // refusal to start — the worst case is the dialog showing once more.
            _ => Self::Ask,
        }
    }
}

/// Which way a split divides its space.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SplitAxisState {
    Horizontal,
    Vertical,
}

impl SplitAxisState {
    fn to_storage(self) -> &'static str {
        match self {
            Self::Horizontal => "h",
            Self::Vertical => "v",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "h" | "horizontal" => Some(Self::Horizontal),
            "v" | "vertical" => Some(Self::Vertical),
            _ => None,
        }
    }
}

/// Which tab is active inside a single group: a main tab or a pinned tab.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GroupActiveState {
    Main(usize),
    Pinned(usize),
}

impl GroupActiveState {
    fn to_storage(self) -> String {
        match self {
            Self::Main(i) => format!("m{i}"),
            Self::Pinned(i) => format!("p{i}"),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('m') {
            rest.parse::<usize>().ok().map(Self::Main)
        } else if let Some(rest) = s.strip_prefix('p') {
            rest.parse::<usize>().ok().map(Self::Pinned)
        } else {
            s.parse::<usize>().ok().map(Self::Main)
        }
    }
}

/// A node in the persisted pane tree: either a tab group or a split.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneNodeState {
    Group {
        main: Vec<(TabRef, f32)>,
        pinned: Vec<PinnedTabRef>,
        active: GroupActiveState,
    },
    Split {
        axis: SplitAxisState,
        ratio: f32,
        left: Box<PaneNodeState>,
        right: Box<PaneNodeState>,
    },
}

impl PaneNodeState {
    fn to_storage(&self) -> String {
        match self {
            Self::Group {
                main,
                pinned,
                active,
            } => {
                let main_s = write_tabs(main);
                let pinned_s = write_pinned_tabs(pinned);
                format!("group({main_s};{pinned_s};{})", active.to_storage())
            }
            Self::Split {
                axis,
                ratio,
                left,
                right,
            } => {
                format!(
                    "split({};{:.3};{};{})",
                    axis.to_storage(),
                    ratio,
                    left.to_storage(),
                    right.to_storage()
                )
            }
        }
    }

    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(inner) = s.strip_prefix("group(") {
            let inner = inner.strip_suffix(')')?;
            let parts = split_at_depth_zero(inner, ';');
            if parts.len() != 3 {
                return None;
            }
            let main_s = parts[0];
            let pinned_s = parts[1];
            let active_s = parts[2];
            let main = if main_s.trim().is_empty() {
                Vec::new()
            } else {
                parse_tabs(main_s)
            };
            let pinned = if pinned_s.trim().is_empty() {
                Vec::new()
            } else {
                pinned_s
                    .split(',')
                    .filter(|e| !e.trim().is_empty())
                    .filter_map(PinnedTabRef::parse)
                    .collect()
            };
            let active = GroupActiveState::parse(active_s).unwrap_or(GroupActiveState::Main(0));
            Some(Self::Group {
                main,
                pinned,
                active,
            })
        } else if let Some(inner) = s.strip_prefix("split(") {
            let inner = inner.strip_suffix(')')?;
            let parts = split_at_depth_zero(inner, ';');
            if parts.len() != 4 {
                return None;
            }
            let axis = SplitAxisState::parse(parts[0].trim())?;
            // `"NaN"` and `"inf"` parse; a NaN survives `clamp` and every
            // pane rect would inherit it. A ratio that is not finite is a
            // corrupt row, and the whole tree falls back to the legacy tabs.
            let ratio = parts[1]
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|r| r.is_finite())?;
            let left = Box::new(Self::parse(parts[2].trim())?);
            let right = Box::new(Self::parse(parts[3].trim())?);
            Some(Self::Split {
                axis,
                ratio: ratio.clamp(0.05, 0.95),
                left,
                right,
            })
        } else {
            None
        }
    }
}

/// Split `s` on `delim`, but only at parenthesis depth zero. This lets nested
/// `split(...)` and `group(...)` nodes contain the delimiter without confusing
/// the parser.
fn split_at_depth_zero(s: &str, delim: char) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut out = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            d if d == delim && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// The persisted split-view tab tree.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneTreeState {
    pub root: PaneNodeState,
    pub focused: Vec<u8>,
}

impl PaneTreeState {
    fn to_storage(&self) -> String {
        let focused = self
            .focused
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("/");
        format!("{}|{focused}", self.root.to_storage())
    }

    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (root_s, focused_s) = s.rsplit_once('|').unwrap_or((s, ""));
        let root = PaneNodeState::parse(root_s)?;
        let focused = if focused_s.trim().is_empty() {
            Vec::new()
        } else {
            focused_s
                .split('/')
                .filter_map(|e| e.trim().parse::<u8>().ok())
                .collect()
        };
        Some(Self { root, focused })
    }
}

fn write_pinned_tabs(pins: &[PinnedTabRef]) -> String {
    pins.iter()
        .map(|p| p.to_storage())
        .collect::<Vec<_>>()
        .join(",")
}

/// The presentation a single document is set to, when it is not following the
/// global default. Each field is `None` to mean "use the global default" — so an
/// entry can override the view mode alone, the page colour alone, or both.
///
/// The colour is RGBA bytes rather than `peniko::Color` because `koru-workspace-host`
/// does not depend on peniko; the document view converts at read time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct DocViewPrefs {
    pub mode: Option<ViewMode>,
    pub canvas_bg: Option<[u8; 4]>,
}

impl DocViewPrefs {
    /// `None` when nothing is overridden, so capture writes no entry at all for
    /// a document that follows the defaults.
    pub fn is_empty(self) -> bool {
        self.mode.is_none() && self.canvas_bg.is_none()
    }
}

/// What this process last wrote to each of the two files it still owns.
///
/// Held by the caller rather than by [`Settings`], because it is a fact about
/// this run — not about the settings — and because a `Settings` that carried it
/// would have it inside its own `PartialEq`.
///
/// Empty means "nothing written yet", so the first save after launch always
/// happens even if the values match what is on disk. That is deliberate: the
/// file may hold another window's idea of the row, and a comparison against
/// what *we* wrote is the only one that stays true when somebody else is
/// writing too.
///
/// **`preferences.conf` is not here any more.** The settings authority owns it,
/// and a patch names the fields it changes rather than serializing a body to
/// diff — which is what the third member of this struct existed to make cheap.
#[derive(Debug, Default)]
pub struct Written {
    profile: Option<String>,
    window: koru_window::WindowRowWriter,
}

/// Which of the two files a save would touch.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub profile: bool,
    pub window_row: bool,
}

impl Changes {
    pub fn any(self) -> bool {
        self.profile || self.window_row
    }
}

impl Written {
    /// A writer for the profile/slot row owned by this window. The root is
    /// already resolved by the launch boundary; the shared crate knows only an
    /// opaque row key and performs every geometry-file write off-thread.
    pub fn for_window(root: Option<&Path>, profile: koru_core::ProfileId, slot: &str) -> Self {
        Self {
            profile: None,
            window: koru_window::WindowRowWriter::open(
                koru_window::window_geometry_path(root),
                window_key(profile, slot),
                "koru workspace window geometry",
            ),
        }
    }

    /// What would be written, given the two rendered bodies.
    ///
    /// **Pure, so it is what the tests assert on** — the alternative is a test
    /// that writes into a real profile directory.
    pub fn changes(&self, profile: &str, window_row: &str) -> Changes {
        Changes {
            profile: self.profile.as_deref() != Some(profile),
            window_row: self.window.would_write(window_row),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Outer position, in physical pixels. `None` lets the OS place the window.
    pub pos: Option<(i32, i32)>,
    /// Where the floating support icon was last left, physical pixels.
    pub floating_pos: Option<(i32, i32)>,
    /// Inner (client) size in logical pixels.
    pub size: (f32, f32),
    /// Whether the window was maximised. Kept separately from `pos`/`size`,
    /// which hold the *restored* geometry — capturing the maximised bounds into
    /// them would leave the window with nothing to un-maximise to.
    pub maximized: bool,
    /// Light/dark intent: System (follow OS), Light, or Dark. Not the resolved
    /// paint flag — that is `theme::is_dark()` after the shell applies this.
    pub theme: ThemePreference,
    /// Editor caret colour (named swatch). Chrome only — not stored in documents.
    pub caret_color: CaretColor,
    /// The chosen icon pack, or `None` to follow the build's default.
    pub icon_pack: Option<koru_draw::icons::IconPack>,
    /// Editor caret stroke width (Thin…8×). Chrome only — not stored in documents.
    pub caret_width: CaretWidth,
    /// Editor caret height (Short / Medium / Tall). Chrome only.
    pub caret_height: CaretHeight,
    /// What the **chrome**'s typeface is set to — tabs, the tree, pills,
    /// panels, the status bar. A setting rather than a face: a role that
    /// follows the default, or names an installed family missing on this
    /// computer, saves as what it is and draws its default (`theme::resolve_font`).
    pub font: FontSetting,
    /// The typeface a document's **prose** rows are set in.
    pub body_font: FontSetting,
    /// The typeface a document's **headings** (H1–H6) are set in.
    pub headings_font: FontSetting,
    /// The typeface **code blocks and inline code** are set in.
    pub code_font: FontSetting,
    /// The document body size the person chose, in px, or `None` while it
    /// follows the default. Documents only; the app's own text has density.
    pub doc_text_size: Option<u8>,
    /// The window's base color — the frame around the panes — chosen for the
    /// light theme, as `0xRRGGBB`, or `None` while it follows the theme.
    pub base_color_light: Option<u32>,
    /// The same for the dark theme.
    pub base_color_dark: Option<u32>,
    /// Display density — Compact, Normal or Comfortable. Shifts the UI and
    /// document-body type roles.
    pub density: Density,
    /// **How the dictation gesture works.** `false` is push-to-talk, which is
    /// the default; `true` is a toggle — press once to start, press again to
    /// stop — which exists for accessibility and for dictation longer than a
    /// comfortable hold, and is what a microphone button will offer.
    ///
    /// It follows the person rather than the folder or the machine, which is
    /// why it is in `preferences.conf` and not in `settings-<slot>.conf`: how
    /// you prefer to hold a key is not a fact about a workspace.
    pub dictation_toggle: bool,
    /// **What language a dictation asks to be transcribed as.**
    ///
    /// [`Requested::Auto`] by default — somebody who has expressed no
    /// preference is not saying *English*, they are saying *work it out* — and
    /// an engine that cannot detect one refuses with `LanguageUnsupported`
    /// rather than transcribing as English anyway.
    ///
    /// Stored as its BCP 47 tag, or `auto`. Absent from an older
    /// `preferences.conf` and from every file written before this key existed,
    /// which reads as the default: that is the whole of the migration, and it
    /// is why the key is written unconditionally from now on.
    pub dictation_language: Requested,
    /// Whether spelling is checked while typing — on by default (D7).
    pub spellcheck_enabled: bool,
    /// The spelling language, or `None` for automatic — the profile's locale.
    /// Stored as its BCP 47 tag, or `auto`.
    pub spellcheck_language: Option<koru_spell::SpellLang>,
    /// **Which quality of model a dictation asks for**, which is never a model
    /// name — see [`ModelTier`].
    ///
    /// [`ModelTier::Compact`] by default, for the tier's own reason: a first
    /// dictation that is quick and slightly wrong is a better introduction
    /// than one that is accurate after a 650 MB download.
    pub dictation_tier: ModelTier,
    /// **Which microphone a dictation asks to listen with**, by name.
    ///
    /// [`InputChoice::SystemDefault`] by default, which is *follow the
    /// operating system* — and is also what somebody who has never opened the
    /// picker gets. Stored as the device's name verbatim, or the fixed token
    /// `system-default`.
    ///
    /// **A name, never a position in a list.** `cpal` enumerates in whatever
    /// order the host offers, and that order moves when a USB interface is
    /// unplugged or a headset sleeps — so an index would silently mean a
    /// different microphone after a reboot. A name that is no longer present
    /// falls back to the system default and the settings section says so; see
    /// [`koru_voice::input`].
    ///
    /// It follows the person for the same reason the other three do: somebody
    /// who dictates through one headset on two machines wants it on both, and
    /// a per-machine key would *forget* the choice rather than recover it.
    pub dictation_input: InputChoice,
    /// Display zoom (1.0 = 100%), clamped to `[0.5, 2.0]`. Whole-window: the
    /// shell applies it as a multiplier on the root transform, so like `dark`
    /// and `font` it is a UI setting rather than workspace data. Unrelated to
    /// `Body::zoom_to` (node navigation).
    pub zoom: f32,
    pub hide_completed: bool,
    /// Window kept above other windows (Ctrl+Shift+T).
    pub pinned: bool,

    // ------------------------------------------------------------- container
    /// The explore column. **Open by default** (the CTO's note): a new
    /// window after Welcome starts with the rail and the pane showing, since
    /// they are the way around. A file that never said otherwise opens too.
    pub sidebar_open: bool,
    /// Which projection the explore column is showing — `"documents"`,
    /// `"files"` or `"tasks"`. The projection is window state, not workspace
    /// content.
    ///
    /// **A token, not a flag.** It was `navigator_files: bool` while there
    /// were two modes, which is exactly what a third cannot be written in. The
    /// legacy key is still read, so a session written by the previous build
    /// reopens where it was, and still written, so one written here reopens
    /// sensibly under that build instead of snapping to Documents. Empty means
    /// nothing was stored, which reopens on Documents.
    pub navigator_mode: String,
    pub context_open: bool,
    /// The independent Koru AI pane. It is window furniture, not document
    /// content, and therefore follows this profile/slot session.
    pub ai_open: bool,
    /// Remembered logical width of the independent Koru AI pane.
    pub ai_w: f32,
    /// Which workspace this window had open. `None` on a first run, and on a
    /// settings file written before there was more than one.
    ///
    /// A separate key rather than part of `tabs` because it names a *document*,
    /// while `tabs` names nodes inside one — and a node id from another
    /// workspace means nothing here, which is why `restore_session` drops the
    /// ones it cannot read.
    pub workspace: Option<WorkspaceId>,
    /// Local Profiles in most-recently-used order for active-Profile removal.
    pub profile_mru: Vec<koru_core::ProfileId>,
    /// The open Main tabs in this window, each with the scroll it was left at.
    ///
    /// **Window-scoped**, like [`pinned_tabs`](Self::pinned_tabs): a workspace
    /// switch does not replace this list. Document entries carry their home
    /// workspace so a tab from elsewhere can reopen without asking Explore to
    /// move first.
    ///
    /// Kept for backward compatibility: a build that does not understand
    /// [`pane_tree`](Self::pane_tree) still opens the focused group from this
    /// flat list.
    pub tabs: Vec<(TabRef, f32)>,
    /// Which of `tabs` was active. Clamped on restore rather than trusted.
    pub tab: usize,
    /// Persisted split-view pane tree. `None` means the session predates split
    /// views and should be read from [`tabs`](Self::tabs) instead.
    pub pane_tree: Option<PaneTreeState>,
    /// Legacy per-workspace Main strips. Still read on load for migration; no
    /// longer written. The live strip is [`tabs`](Self::tabs).
    pub sessions: Vec<(WorkspaceId, Vec<(TabRef, f32)>, usize)>,
    /// Documents opened, most recent first. What the welcome tab offers.
    ///
    /// **Stable ids, not workspace nodes.** A `DocumentId` is what the store
    /// catalogues across every workspace, so a recent survives a workspace
    /// switch and is resolved through the store rather than against whichever
    /// document happens to be under the caret.
    pub recents: Vec<DocumentId>,
    /// Pinned document tabs in this window, in strip order.
    pub pinned_tabs: Vec<PinnedTabRef>,
    /// Which pin is showing, when the active tab is a pin.
    pub pinned_tab: Option<usize>,
    /// Welcome default: newly pinned tabs show their title.
    pub pinned_show_title: bool,
    /// **The terminal's theme and font, per agent** (ADR-475): `claude`,
    /// `codex`, `powershell`, or `custom:<id>` to the look that agent's tabs
    /// use instead of the Settings page's. A key absent here follows Settings.
    pub terminal_looks: std::collections::BTreeMap<String, TerminalLook>,
    /// Nodes whose children are hidden in the explore pane, **per workspace**.
    ///
    /// Collapsed is the *exception* — a newly-seen node appears rather than
    /// having to be found — so this is what somebody has deliberately shut, and
    /// losing it is losing that work.
    ///
    /// **Keyed by workspace, unlike everything else here.** A `NodeId` is only
    /// meaningful inside the document that minted it, so one flat list meant
    /// that switching workspaces both discarded the set you left behind and
    /// restored it into a document where every id named nothing — which looked,
    /// from the outside, exactly like the tree re-expanding on every switch. A
    /// profile has many workspaces and you move between them; a single slot is
    /// the wrong grain for anything holding node ids.
    ///
    /// Filtered against the live tree on the way out rather than trusted on the
    /// way in, so a node collapsed and then deleted does not sit here forever.
    pub collapsed: Vec<(WorkspaceId, Vec<NodeId>)>,
    /// Whether the workspace manager is showing archived workspaces. One flag
    /// for the window rather than one per workspace: it is about how you are
    /// looking at the list, not about anything in it.
    pub show_archived: bool,
    /// How wide the explore pane is, per workspace. Absent means the default.
    pub sidebar_w: Vec<(WorkspaceId, f32)>,
    /// When this machine last opened each workspace, unix milliseconds.
    pub opened: Vec<(WorkspaceId, i64)>,
    /// The global default document view mode and page background, in Roaming.
    /// A document with no [`DocViewPrefs`] entry of its own follows these.
    pub view_mode: ViewMode,
    /// `None` means "follow the theme surface" — see [`DocViewPrefs::canvas_bg`].
    pub canvas_bg: Option<[u8; 4]>,
    /// Per-document presentation overrides, keyed by workspace for the same
    /// reason [`collapsed`](Self::collapsed) is: a `NodeId` is only meaningful
    /// inside the document that minted it.
    pub doc_view_prefs: Vec<(WorkspaceId, Vec<(NodeId, DocViewPrefs)>)>,
    /// What closing a popped-out document window does, once remembered. The
    /// container's pop-out and the document window's close dialog are the only
    /// readers; `Ask` means the dialog is shown.
    pub popout_close: PopoutClose,
    /// Unicode emoji selected most recently, per user rather than per
    /// workspace. Kept out of Loro because usage ranking is a preference, not
    /// collaborative document state.
    pub emoji_recents: Vec<String>,
    /// Preferred Fitzpatrick tone applied when inserting a tone-capable emoji
    /// from the picker. Token names match `koru_editor::emoji::skin_tone_token`.
    pub emoji_skin_tone: String,
    // **`update_channel` and `automatic_update_checks` are gone, and nothing
    // migrates them.** Updating belongs to `koru-agent` now: the channel is a
    // fact about which binary this is rather than something to pick, and whether
    // to check automatically is the agent's setting because the agent is what
    // checks. The authority's parser ignores unknown keys, so an existing
    // `preferences.conf` carrying `update_channel=nightly` is read, ignored, and
    // dropped by the next save — which is the whole of the migration and is a
    // decision rather than an oversight. Carrying the value forward would
    // preserve a preference that cannot have an effect, and the first person to
    // notice would find a channel selector in a file with nothing to select.
    /// **What ✕ on the last window does.** Device-local — a machine with no
    /// tray cannot keep Koru resident — but a *preference* all the same, so it
    /// is the registry's and arrives here from the authority like the rest.
    /// It used to be read and written straight to `desktop-v1.json` by
    /// whichever window happened to be open, each save serializing the whole
    /// body over the others'.
    pub close_behavior: koru_window::CloseBehavior,
    /// Directory last used for a document (or workspace) export file dialog.
    ///
    /// Follows the user in `preferences.conf`. Absent until the first successful
    /// export; cancel does not write it.
    pub last_export_dir: Option<PathBuf>,
    /// **What the person has seen of each AI chat, on this device** (koru-ai
    /// D83): the `chat_seen=` line, opaque here — `koru_chats_ui::SeenBook`
    /// reads and writes it. Device-local by the person's choice, so it lives
    /// in this session file beside the tabs rather than in a synced setting.
    ///
    /// **An older build ignores the line** (`from_text`'s `_ => {}`) and drops
    /// it on its next save — every chat then reads as seen, which is the safe
    /// way to be wrong. Empty writes no line.
    pub chat_seen: String,
    /// **The order of the views down the rail, on this device**: the
    /// `rail_order=` line, a comma list of view tokens, opaque here —
    /// `apps/koru`'s `panes::sidebar::rail_order` reads and writes it, and
    /// merges it with the views that build knows. Device-local, like
    /// [`chat_seen`](Self::chat_seen): how one machine's rail is arranged is
    /// that machine's furniture.
    ///
    /// **An older build ignores the line** (`from_text`'s `_ => {}`), draws
    /// its default order, and drops the line on its next save. Empty — the
    /// default order — writes no line.
    pub rail_order: String,
}

/// Settings loaded before the Store has supplied a real Profile identity.
///
/// The values are intentionally not exposed as [`Settings`]. Startup may read
/// machine/person preferences and the remembered workspace hint, but views and
/// persistence receive [`ProfileSettings`] only after [`bind`](Self::bind)
/// consumes this value with a real [`ProfileId`](koru_core::ProfileId). This
/// is the staging boundary Plan 3.3's zero-Profile shell relies on: it cannot
/// accidentally restore tabs or write a Profile session before one exists.
pub(crate) struct PreIdentitySettings {
    values: Settings,
}

/// The preference-only view available before content identity exists.
///
/// Profile geometry, workspace and tab/session fields are absent by type, so
/// startup cannot accidentally apply them while it is still identity-free.
pub(crate) struct PreIdentityPreferences {
    pub(crate) theme: ThemePreference,
    pub(crate) caret_color: CaretColor,
    pub(crate) icon_pack: Option<koru_draw::icons::IconPack>,
    pub(crate) caret_width: CaretWidth,
    pub(crate) caret_height: CaretHeight,
    pub(crate) font: FontSetting,
    pub(crate) body_font: FontSetting,
    pub(crate) headings_font: FontSetting,
    pub(crate) code_font: FontSetting,
    pub(crate) doc_text_size: Option<u8>,
    pub(crate) base_color_light: Option<u32>,
    pub(crate) base_color_dark: Option<u32>,
    pub(crate) density: Density,
    pub(crate) dictation_toggle: bool,
    pub(crate) dictation_language: Requested,
    pub(crate) dictation_tier: ModelTier,
    pub(crate) dictation_input: InputChoice,
    pub(crate) spellcheck_enabled: bool,
    pub(crate) spellcheck_language: Option<koru_spell::SpellLang>,
    pub(crate) zoom: f32,
    pub(crate) view_mode: ViewMode,
    pub(crate) canvas_bg: Option<[u8; 4]>,
}

impl PreIdentitySettings {
    /// Load preferences and any existing session hints without requiring or
    /// fabricating a Profile id.
    ///
    /// This phase is read-only. A legacy preferences rewrite is deferred until
    /// a real Profile has been created and bound, so opening/cancelling first
    /// run cannot write merely because an old preference spelling was present.
    pub(crate) fn load(profile_dir: &Path, peer: &str) -> Self {
        let values = Settings::load(profile_dir, peer);
        Self::from_settings(values)
    }

    fn from_settings(values: Settings) -> Self {
        Self { values }
    }

    /// Preferences which are safe to apply before a content identity exists.
    ///
    /// Profile/session state in the same transitional file is not present in
    /// the returned type and is not handed to a view until `bind`.
    pub(crate) fn preferences(&self) -> PreIdentityPreferences {
        PreIdentityPreferences {
            theme: self.values.theme,
            caret_color: self.values.caret_color,
            icon_pack: self.values.icon_pack,
            caret_width: self.values.caret_width,
            caret_height: self.values.caret_height,
            font: self.values.font.clone(),
            body_font: self.values.body_font.clone(),
            headings_font: self.values.headings_font.clone(),
            code_font: self.values.code_font.clone(),
            doc_text_size: self.values.doc_text_size,
            base_color_light: self.values.base_color_light,
            base_color_dark: self.values.base_color_dark,
            density: self.values.density,
            dictation_toggle: self.values.dictation_toggle,
            dictation_language: self.values.dictation_language,
            dictation_tier: self.values.dictation_tier,
            dictation_input: self.values.dictation_input.clone(),
            spellcheck_enabled: self.values.spellcheck_enabled,
            spellcheck_language: self.values.spellcheck_language,
            zoom: self.values.zoom,
            view_mode: self.values.view_mode,
            canvas_bg: self.values.canvas_bg,
        }
    }

    /// The existing launch hint, consulted while the Store chooses content.
    ///
    /// This does not restore a session; it only preserves the current launch's
    /// workspace-selection behavior until settings move into the Store.
    pub(crate) fn workspace_hint(&self) -> Option<WorkspaceId> {
        self.values.workspace
    }

    /// Bind the staged values to a real Profile and apply its keyed geometry.
    pub(crate) fn bind(
        mut self,
        profile: koru_core::ProfileId,
        slot: &str,
        root: Option<&Path>,
    ) -> ProfileSettings {
        self.values.apply_window_state(profile, slot, root);
        ProfileSettings {
            profile,
            values: self.values,
        }
    }

    #[cfg(test)]
    fn bind_with_window_row(
        mut self,
        profile: koru_core::ProfileId,
        row: Option<&str>,
    ) -> ProfileSettings {
        if let Some(row) = row {
            self.values.apply_window_row(row);
        }
        ProfileSettings {
            profile,
            values: self.values,
        }
    }
}

/// The settings/session state of one real Profile.
///
/// Keeping this as a distinct owner rather than optional fields makes the
/// identity boundary structural while preserving the established `Settings`
/// API used by views.
pub(crate) struct ProfileSettings {
    profile: koru_core::ProfileId,
    values: Settings,
}

impl ProfileSettings {
    pub(crate) fn profile_id(&self) -> koru_core::ProfileId {
        self.profile
    }
}

impl Deref for ProfileSettings {
    type Target = Settings;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl DerefMut for ProfileSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}

/// The size a window opens at when nothing has been remembered — and what
/// *Reset Window Position* puts it back to.
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (940.0, 620.0);

/// Smallest inner size the main window may be resized to.
///
/// 720, not the old 500: the shared banner's arithmetic (SP-0 of the
/// UI-refactor program) showed that at 500 with the explore pane open the
/// caption's slots and window controls leave no room for one minimum-width
/// tab. 720 matches the Terminal's floor, so the shared chrome has one
/// honest minimum where every element fits (owner decision, 2026-08-31).
pub const MIN_WINDOW_SIZE: (f32, f32) = (720.0, 600.0);

/// Where a session recording goes when the person has not chosen a folder
/// yet (SESSIONRECORD): the user's Videos folder, else their home directory.
/// `None` when the OS will not say either — the modal then asks outright.
pub fn videos_dir() -> Option<std::path::PathBuf> {
    let dirs = directories::UserDirs::new()?;
    dirs.video_dir()
        .map(std::path::Path::to_path_buf)
        .or_else(|| Some(dirs.home_dir().to_path_buf()))
}

/// Whether an OS-reported window position can be remembered or restored.
///
/// Windows uses a point around `(-32000, -32000)` while a window is minimised.
/// A real multi-monitor desktop can have negative coordinates, but not both
/// axes this far away from the primary display.
pub(crate) fn usable_window_position(x: i32, y: i32) -> bool {
    x > -10_000 || y > -10_000
}

/// The remembered position a new window may open at, given the monitors that
/// exist now.
///
/// **A remembered position is a hint about a desktop that may no longer be
/// there.** It was saved in physical pixels on whatever screens were connected
/// at the time, so an unplugged monitor, a dropped resolution, a dock left
/// behind or an RDP session with different screens all turn it into
/// coordinates that describe nothing — the window opens off-screen and only
/// the taskbar ever sees it. [`usable_window_position`] rejects the minimised
/// sentinel; this asks the live monitor set whether the window would land
/// where its caption can be grabbed ([`koru_window::snap::reachable`]), and
/// answers `None` — the OS's default placement — when it would not.
///
/// `size` is the remembered *logical* size and `scale` the primary monitor's
/// factor. The position is exact; the size only has to be close enough to ask
/// a fair question of a caption-sized tolerance, which a mixed-DPI desktop
/// cannot be more precise about anyway.
pub(crate) fn restorable_position(
    pos: Option<(i32, i32)>,
    size: (f32, f32),
    scale: f64,
    monitors: &[(i32, i32, i32, i32)],
) -> Option<(i32, i32)> {
    let (x, y) = pos.filter(|&(x, y)| usable_window_position(x, y))?;
    // Float-to-int `as` saturates (NaN → 0), and the parsed range keeps this
    // far below the `u32` it feeds.
    let physical = (
        (f64::from(size.0) * scale).round() as u32,
        (f64::from(size.1) * scale).round() as u32,
    );
    koru_window::snap::reachable(monitors, (x, y), physical).then_some((x, y))
}

/// How many recents are kept. A list long enough to scroll is a file browser,
/// which is a different feature.
pub const MAX_RECENTS: usize = 8;

/// How stale the *opened at* stamp for the open workspace may get before an
/// idle tick refreshes it. See
/// [`refresh_opened_in`](Settings::refresh_opened_in) for why this is not zero.
const OPENED_REFRESH_MS: i64 = 5 * 60 * 1000;

impl Default for Settings {
    fn default() -> Self {
        Self {
            pos: None,
            floating_pos: None,
            workspace: None,
            size: DEFAULT_WINDOW_SIZE,
            maximized: false,
            theme: ThemePreference::System,
            caret_color: CaretColor::Default,
            icon_pack: None,
            caret_width: CaretWidth::Medium,
            caret_height: CaretHeight::Medium,
            // The theme owns the per-role defaults — these and the theme's
            // startup atomics are two homes of one answer, held together by
            // `theme::tests::defaults_and_indices_agree`.
            font: FontSetting::FollowDefault,
            body_font: FontSetting::FollowDefault,
            headings_font: FontSetting::FollowDefault,
            code_font: FontSetting::FollowDefault,
            doc_text_size: None,
            base_color_light: None,
            base_color_dark: None,
            density: Density::default(),
            dictation_toggle: false,
            dictation_language: Requested::Auto,
            spellcheck_enabled: true,
            spellcheck_language: None,
            dictation_tier: ModelTier::default(),
            dictation_input: InputChoice::SystemDefault,
            zoom: 1.0,
            hide_completed: false,
            pinned: false,
            sidebar_open: true,
            navigator_mode: String::new(),
            context_open: false,
            ai_open: false,
            ai_w: 480.0,
            profile_mru: Vec::new(),
            tabs: Vec::new(),
            tab: 0,
            pane_tree: None,
            sessions: Vec::new(),
            recents: Vec::new(),
            pinned_tabs: Vec::new(),
            pinned_tab: None,
            pinned_show_title: false,
            terminal_looks: std::collections::BTreeMap::new(),
            collapsed: Vec::new(),
            show_archived: false,
            sidebar_w: Vec::new(),
            opened: Vec::new(),
            // Narrow, the registry's default (`ViewModeChoice`), not the
            // draw enum's Full, which is the outline's only mode.
            view_mode: ViewMode::Narrow,
            canvas_bg: None,
            doc_view_prefs: Vec::new(),
            popout_close: PopoutClose::default(),
            emoji_recents: Vec::new(),
            // Empty, not `"default"` — the registry's default, which
            // `koru_editor::emoji::parse_skin_tone` reads as the default tone
            // anyway. `crate::preferences::tests` pins the two together, so a
            // window that has not heard from the authority yet is showing
            // exactly what the authority would have told it.
            emoji_skin_tone: String::new(),
            close_behavior: koru_window::CloseBehavior::default(),
            last_export_dir: None,
            chat_seen: String::new(),
            rail_order: String::new(),
        }
    }
}

/// `<profile_dir>/settings-<peer>.conf`.
///
/// **Inside the data directory, not beside it.** Content and session in two
/// separate folders would let two builds be pointed at different content and
/// still fight over one set of tabs and one window position. A data directory
/// that holds everything is also one you can copy to another machine and have
/// arrive intact.
///
/// One file per peer, so two windows opened side by side keep their own
/// geometry instead of overwriting each other every frame.
fn config_path(profile_dir: &Path, peer: &str) -> PathBuf {
    profile_dir.join(format!("settings-{peer}.conf"))
}

// ----------------------------------------------- one struct, two files, one API
//
// **Every view still sees one `Settings`** — nothing about `restore` or
// `capture` changed — but each field goes somewhere different, and only two of
// those places are files this crate opens:
//
// | | |
// | --- | --- |
// | the settings authority | **you**: theme, the four font roles, density, zoom, caret, popout, view defaults, dictation, and what ✕ on the last window does. Asked for and patched through `crate::preferences`; **no file here** |
// | `%LOCALAPPDATA%\koru\windows.conf` | **this machine**: geometry, per (profile, slot) |
// | `<profile_dir>/settings-<slot>.conf` | **your content**: workspace, tabs, recents, columns — and the emoji ranking and last export directory, which came *back* from `preferences.conf` because neither is a preference |
//
// The split is what stops work being light while personal is dark, and stops a
// window position from a three-monitor desktop arriving on a laptop through a
// synced folder. What the authority added on top is that two windows agree:
// before it, each one serialized the whole preferences file and the last save
// won (ADR-266).

/// The key each workspace's collapsed set is filed under. The workspace's own
/// uuid follows, so two workspaces cannot collide and a workspace that is gone
/// simply has a row nothing reads.
const COLLAPSED_PREFIX: &str = "collapsed.";

/// The key each workspace's tab strip is filed under, the same shape as
/// [`COLLAPSED_PREFIX`] and for the same reason.
const SESSION_PREFIX: &str = "session.";

/// One workspace's tab strip, from `<entry>,<entry>`.
///
/// An entry that will not parse is skipped on its own: losing one tab is
/// survivable, losing the session is not.
fn parse_tabs(value: &str) -> Vec<(TabRef, f32)> {
    value
        .split(',')
        .filter(|e| !e.trim().is_empty())
        .filter_map(|entry| {
            let entry = entry.trim();
            // **`rsplit_once`, not `split_once`.** A document is written
            // `doc:<uuid>`, so the *first* colon in an entry belongs to the
            // reference rather than to the scroll — splitting there read the
            // reference as `doc` and dropped every document tab. A tail that is
            // not a number belongs to the reference too, so a hand-written
            // entry with no scroll still comes back.
            let (which, scroll) = match entry.rsplit_once(':') {
                Some((w, sc)) => match sc.parse::<f32>() {
                    Ok(v) => (w, v),
                    Err(_) => (entry, 0.0),
                },
                None => (entry, 0.0),
            };
            // A NaN or infinite scroll would poison every later clamp; treat it
            // as the top.
            let scroll = if scroll.is_finite() {
                scroll.max(0.0)
            } else {
                0.0
            };
            TabRef::parse(which).map(|t| (t, scroll))
        })
        .collect()
}

/// One workspace's tab strip, as it is written.
fn write_tabs(tabs: &[(TabRef, f32)]) -> String {
    tabs.iter()
        .map(|(t, scroll)| format!("{}:{scroll:.0}", t.to_storage()))
        .collect::<Vec<_>>()
        .join(",")
}

/// The key each workspace's per-document view prefs are filed under, the same
/// shape as [`COLLAPSED_PREFIX`] for the same reason.
const DOCPREF_PREFIX: &str = "docpref.";

/// The key each workspace's explore-pane width is filed under. Same shape and
/// same reasoning as [`COLLAPSED_PREFIX`]: how wide you want the pane depends on
/// what is in the workspace, so it belongs per workspace rather than per window.
const SIDEBAR_W_PREFIX: &str = "sidebar_w.";

/// When this machine last opened each workspace. Local by nature — "when *you*
/// last looked at it" is not a fact about the workspace, and must not be a
/// CRDT write a peer overwrites.
const OPENED_PREFIX: &str = "opened.";

/// What a stored pane width is allowed to be.
///
/// Wider than the drag's own limits on purpose, so a value written by a build
/// with different limits still loads — but bounded, because a corrupt line must
/// not be able to make the window unusable. Exactly the stance the window
/// geometry takes.
const SIDEBAR_W_RANGE: std::ops::RangeInclusive<f32> = 120.0..=1200.0;

// ----------------------------------------------------- view-mode / page-bg tokens
//
// The on-disk spellings for an optional `ViewMode` and page colour, used by the
// per-document overrides in the profile file. The *global* defaults these
// override are the settings registry's, spelled once in `koru-settings`; the
// copies that used to live here went with the preferences writer.

fn view_mode_token_opt(mode: Option<ViewMode>) -> &'static str {
    match mode {
        Some(m) => match m {
            ViewMode::Page => "p",
            ViewMode::Narrow => "n",
            ViewMode::Full => "f",
        },
        None => "-",
    }
}

fn parse_view_mode_opt(s: &str) -> Option<ViewMode> {
    match s.trim() {
        "p" => Some(ViewMode::Page),
        "n" => Some(ViewMode::Narrow),
        "f" => Some(ViewMode::Full),
        _ => None,
    }
}

/// The per-document override spelling: a hex string, or `-` for unset.
fn canvas_bg_token_opt(bg: Option<[u8; 4]>) -> String {
    match bg {
        Some([r, g, b, _]) => format!("{:02x}{:02x}{:02x}", r, g, b),
        None => "-".to_string(),
    }
}

fn parse_canvas_bg_opt(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    if s == "-" {
        return None;
    }
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(s.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(s.get(4..6)?, 16).ok()?;
    Some([r, g, b, 255])
}

/// One `docpref` value entry: `<node>|<mode>|<bg>`. Drops itself if the node or
/// every field is unparseable — losing one override is survivable.
fn parse_doc_pref_entry(s: &str) -> Option<(NodeId, DocViewPrefs)> {
    let mut parts = s.trim().split('|');
    let node = NodeId::parse(parts.next()?)?;
    let mode = parts.next().and_then(parse_view_mode_opt);
    let canvas_bg = parts.next().and_then(parse_canvas_bg_opt);
    let prefs = DocViewPrefs { mode, canvas_bg };
    (!prefs.is_empty()).then_some((node, prefs))
}

/// Preferences: Roaming, because they describe you rather than this machine.
const PREFERENCES_FILE: &str = "preferences.conf";
/// Window geometry: **Local**, because it describes this machine's screens. XDG
/// names window layout as state for exactly this reason.
#[cfg(target_os = "macos")]
const WINDOWS_FILE: &str = "windows.conf";

/// Where an early macOS build filed these, before the move to
/// `~/Library/Application Support`.
///
/// Read-only, and only when the real file is absent. **Nothing is deleted** —
/// the same rule recovery follows, and it is what makes the move safe to reverse
/// by reverting the code.
#[cfg(target_os = "macos")]
fn legacy_xdg(file: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if file == PREFERENCES_FILE {
        home.join(".config").join("koru").join(file)
    } else {
        home.join(".local/state").join("koru").join(file)
    })
}

/// The file to read, preferring the real location and falling back to where an
/// older build left one. Writes always go to the real location, so the first
/// save completes the migration.
fn read_conf(path: &Path, file: &str) -> Option<String> {
    if let Ok(text) = fs::read_to_string(path) {
        return Some(text);
    }
    #[cfg(target_os = "macos")]
    if let Some(old) = legacy_xdg(file)
        && let Ok(text) = fs::read_to_string(&old)
    {
        return Some(text);
    }
    let _ = file;
    None
}

/// **One process decides where these files are, and it is not this one.**
/// `koru-window` owns both paths because the agent is a separate process that
/// reads `preferences.conf` for its caret before any window exists — two
/// resolvers would be two quietly different meanings for one file.
///
/// That is why the `config_base`/`state_base` pair that used to live here is
/// gone rather than merged: it answered the same question a second time. The
/// macOS half of that answer — that both files belong in
/// `~/Library/Application Support`, since the platform has no roaming/local
/// split and no XDG state directory — made the journey with ADR-116's
/// implementation, so [`read_conf`]'s fallback to the old XDG place *is* the
/// migration now: reads prefer the new location, writes always go there, and
/// the first save completes the move. Delete `legacy_xdg` only once that
/// fallback has shipped long enough to be pointless.
fn settings_files(profile_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(profile_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_settings = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("settings-") && name.ends_with(".conf"));
        if is_settings {
            files.push(path);
        }
    }
    Ok(files)
}
pub(crate) fn cleanup_removed_profile_state(
    profile_dir: &Path,
    root: Option<&Path>,
    profile: koru_core::ProfileId,
    remaining_workspaces: &std::collections::HashSet<WorkspaceId>,
) -> std::io::Result<()> {
    for path in settings_files(profile_dir)? {
        let body = fs::read_to_string(&path)?;
        let mut settings = Settings::from_text(&body);
        settings.retain_workspace_state(remaining_workspaces);
        settings
            .profile_mru
            .retain(|candidate| *candidate != profile);
        fs::write(path, settings.profile_body())?;
    }
    if let Some(path) = windows_path(root)
        && path.is_file()
    {
        let body = fs::read_to_string(&path)?;
        let prefix = format!("{} |", profile.to_storage());
        let kept = body
            .lines()
            .filter(|line| !line.trim_start().starts_with(&prefix))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{kept}\n"))?;
    }
    Ok(())
}
pub(crate) fn reconcile_removed_profile_state(
    profile_dir: &Path,
    root: Option<&Path>,
    live_profiles: &std::collections::HashSet<koru_core::ProfileId>,
    live_workspaces: &std::collections::HashSet<WorkspaceId>,
) -> std::io::Result<()> {
    for path in settings_files(profile_dir)? {
        let body = fs::read_to_string(&path)?;
        let mut settings = Settings::from_text(&body);
        settings.retain_workspace_state(live_workspaces);
        settings.retain_profiles(live_profiles);
        let next = settings.profile_body();
        if next != body {
            fs::write(path, next)?;
        }
    }
    if let Some(path) = windows_path(root)
        && path.is_file()
    {
        let body = fs::read_to_string(&path)?;
        let next = retain_live_profile_window_rows(&body, live_profiles);
        if next != body {
            fs::write(path, next)?;
        }
    }
    Ok(())
}

fn retain_live_profile_window_rows(
    body: &str,
    live_profiles: &std::collections::HashSet<koru_core::ProfileId>,
) -> String {
    let mut kept = body
        .lines()
        .filter(|line| {
            let Some((profile, _)) = line.trim_start().split_once(" |") else {
                return true;
            };
            koru_core::ProfileId::parse(profile.trim())
                .is_none_or(|profile| live_profiles.contains(&profile))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !kept.is_empty() || body.ends_with('\n') {
        kept.push('\n');
    }
    kept
}
fn preferences_path() -> Option<PathBuf> {
    koru_window::user_preferences_path()
}

/// How many emoji the picker's usage ranking keeps.
pub const MAX_EMOJI_RECENTS: usize = 32;

/// **The one-time move of `emoji_recents` and `last_export_dir`.**
///
/// Neither is a preference — one is a ranking the picker derives from use, the
/// other is where a file dialog last was — so ADR-266 relocates both into the
/// profile-slot file this host already owns, leaving `preferences.conf` with a
/// single reader and a single writer.
///
/// **This is the only place in the crate that still opens that file, it runs
/// only for a session that has neither key yet, and it never writes.** It is
/// the same shape as the macOS XDG fallback: read the old home once, write the
/// new one, and delete this once no install predates the move. `crate::preferences::tests` pins that it is the only one.
fn migrate_relocated_keys(into: &mut Settings) {
    if !into.emoji_recents.is_empty() || into.last_export_dir.is_some() {
        return;
    }
    let Some(text) = preferences_path().and_then(|path| read_conf(&path, PREFERENCES_FILE)) else {
        return;
    };
    for (key, value) in key_values(&text) {
        match key {
            "emoji_recents" => {
                into.emoji_recents = value
                    .split(',')
                    .filter(|item| !item.is_empty())
                    .take(MAX_EMOJI_RECENTS)
                    .map(str::to_owned)
                    .collect();
            }
            "last_export_dir" => {
                into.last_export_dir = (!value.is_empty()).then(|| PathBuf::from(value));
            }
            _ => {}
        }
    }
}

fn windows_path(root: Option<&Path>) -> Option<PathBuf> {
    koru_window::window_geometry_path(root)
}

/// Where a window's geometry is filed: one row per profile *and* slot, so two
/// profiles open side by side keep their own places.
fn window_key(profile: koru_core::ProfileId, slot: &str) -> String {
    format!("{} | {}", profile.to_storage(), slot)
}

fn load_window_row(root: Option<&Path>, key: &str) -> Option<String> {
    if let Some(row) = windows_path(root).and_then(|path| koru_window::read_window_row(&path, key))
    {
        return Some(row);
    }
    // Preserve ADR-116's one-way macOS migration for ordinary launches. A
    // rooted run is an isolation boundary and must never fall back outside it.
    #[cfg(target_os = "macos")]
    if root.is_none() {
        return legacy_xdg(WINDOWS_FILE).and_then(|path| koru_window::read_window_row(&path, key));
    }
    None
}

impl Settings {
    pub(crate) fn remember_profile(&mut self, profile: koru_core::ProfileId) {
        self.profile_mru.retain(|candidate| *candidate != profile);
        self.profile_mru.insert(0, profile);
    }

    fn retain_profiles(&mut self, live_profiles: &std::collections::HashSet<koru_core::ProfileId>) {
        self.profile_mru
            .retain(|profile| live_profiles.contains(profile));
    }
    pub(crate) fn retain_workspace_state(
        &mut self,
        remaining: &std::collections::HashSet<WorkspaceId>,
    ) {
        if self.workspace.is_some_and(|id| !remaining.contains(&id)) {
            self.workspace = None;
        }
        self.sessions.retain(|(id, _, _)| remaining.contains(id));
        self.collapsed.retain(|(id, _)| remaining.contains(id));
        self.sidebar_w.retain(|(id, _)| remaining.contains(id));
        self.opened.retain(|(id, _)| remaining.contains(id));
        self.doc_view_prefs.retain(|(id, _)| remaining.contains(id));
        self.tabs.retain(|(tab, _)| match tab {
            TabRef::Document {
                workspace: Some(workspace),
                ..
            } => remaining.contains(workspace),
            _ => true,
        });
        self.pinned_tabs
            .retain(|tab| remaining.contains(&tab.workspace));
        self.tab = self.tab.min(self.tabs.len().saturating_sub(1));
        self.pinned_tab = self
            .pinned_tab
            .filter(|index| *index < self.pinned_tabs.len());
    }

    /// The layout a document opens with: its own [`DocViewPrefs`] where it has
    /// one in the current workspace, the global default for every field it
    /// does not override. `None` is a document with no root to look up — a
    /// draft, or a page rooted in a private workspace — which simply follows
    /// the defaults.
    ///
    /// **The one resolution, used by every reader.** `restore` and the startup
    /// adoption both go through here, so a document cannot be restored by one
    /// rule and re-resolved by another.
    pub fn doc_layout(&self, root: Option<NodeId>) -> (ViewMode, Option<[u8; 4]>) {
        let prefs = root.and_then(|root| self.doc_view_prefs_for(root));
        (
            prefs.and_then(|p| p.mode).unwrap_or(self.view_mode),
            prefs.and_then(|p| p.canvas_bg).or(self.canvas_bg),
        )
    }

    /// The override recorded for `root` in the current workspace, if any.
    pub fn doc_view_prefs_for(&self, root: NodeId) -> Option<DocViewPrefs> {
        let ws = self.workspace?;
        self.doc_view_prefs
            .iter()
            .find(|(w, _)| *w == ws)
            .and_then(|(_, entries)| entries.iter().find(|(n, _)| *n == root))
            .map(|(_, p)| *p)
    }

    /// What is collapsed in `workspace`. Empty for one nobody has shut anything
    /// in, which is the same answer as never having opened it.
    pub fn collapsed_in(&self, workspace: WorkspaceId) -> &[NodeId] {
        self.collapsed
            .iter()
            .find(|(id, _)| *id == workspace)
            .map_or(&[], |(_, nodes)| nodes.as_slice())
    }

    /// Record what is collapsed in `workspace`, leaving every other workspace's
    /// set alone. **That is the whole point** — writing the field directly is
    /// what discarded the workspace you had just left.
    pub fn set_collapsed_in(&mut self, workspace: WorkspaceId, nodes: Vec<NodeId>) {
        match self.collapsed.iter_mut().find(|(id, _)| *id == workspace) {
            Some(slot) => slot.1 = nodes,
            None => self.collapsed.push((workspace, nodes)),
        }
    }

    /// No-op kept for call sites that still name the old per-workspace strip.
    ///
    /// The Main strip is window-scoped; capture writes `tabs` directly.
    pub fn remember_tabs_of(&mut self, _workspace: WorkspaceId) {}

    /// No-op: switching workspace must not replace the open strip.
    pub fn adopt_tabs_of(&mut self, _workspace: WorkspaceId) {}

    /// Every document still open in this window (Main + pinned, in every
    /// column), for parking rooms across a workspace switch.
    pub fn open_document_ids(&self) -> Vec<DocumentId> {
        self.remembered_documents()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Every document a remembered tab is over, once each, with the workspace
    /// it lives in when the entry names one.
    ///
    /// **Every group of [`pane_tree`](Self::pane_tree), not only `tabs`.**
    /// `tabs` and `pinned_tabs` are the *focused* group's copy, kept for older
    /// builds; a second column's documents are in the tree and nowhere else.
    /// Reading only the flat lists meant a document in the column that did not
    /// have focus was never opened before restore, so `restore_session` found
    /// nothing to rebuild it from and the column came back as Welcome.
    pub fn remembered_documents(&self) -> Vec<(DocumentId, Option<WorkspaceId>)> {
        fn group_documents(node: &PaneNodeState, out: &mut Vec<(DocumentId, Option<WorkspaceId>)>) {
            match node {
                PaneNodeState::Group { main, pinned, .. } => {
                    // A chat entry, pinned or not, is the AI service's to
                    // reopen: nothing here asks the store for it.
                    for pin in pinned {
                        if let Some(id) = pin.document() {
                            out.push((id, Some(pin.workspace)));
                        }
                    }
                    for (which, _) in main {
                        if let TabRef::Document { id, workspace } = which {
                            out.push((*id, *workspace));
                        }
                    }
                }
                PaneNodeState::Split { left, right, .. } => {
                    group_documents(left, out);
                    group_documents(right, out);
                }
            }
        }
        let mut all = Vec::new();
        for (which, _) in &self.tabs {
            if let TabRef::Document { id, workspace } = which {
                all.push((*id, *workspace));
            }
        }
        for pin in &self.pinned_tabs {
            if let Some(id) = pin.document() {
                all.push((id, Some(pin.workspace)));
            }
        }
        if let Some(tree) = &self.pane_tree {
            group_documents(&tree.root, &mut all);
        }
        let mut out: Vec<(DocumentId, Option<WorkspaceId>)> = Vec::new();
        for (id, home) in all {
            match out.iter_mut().find(|(seen, _)| *seen == id) {
                // A later entry may know the home an earlier legacy one lacked.
                Some(slot) => slot.1 = slot.1.or(home),
                None => out.push((id, home)),
            }
        }
        out
    }

    /// Forget every workspace not in `live`.
    ///
    /// Without this the list grows a row for every workspace this profile has
    /// ever had, naming nodes in documents that are gone — the same slow leak of
    /// ids that `ExploreView::collapsed` filters out one level down, at the
    /// grain above it.
    pub fn prune_collapsed(&mut self, live: &[WorkspaceId]) {
        self.collapsed.retain(|(id, _)| live.contains(id));
        // Every per-workspace list is pruned here, not just the collapsed one —
        // otherwise each grows a row for every workspace this profile has ever
        // had. **An archived workspace counts as live**, or archiving would
        // throw away its collapsed set and its pane width and unarchiving would
        // not bring them back.
        self.sidebar_w.retain(|(id, _)| live.contains(id));
        self.opened.retain(|(id, _)| live.contains(id));
        self.sessions.retain(|(id, _, _)| live.contains(id));
    }

    /// When this machine last opened `workspace`, if it ever has.
    pub fn opened_in(&self, workspace: WorkspaceId) -> Option<i64> {
        self.opened
            .iter()
            .find(|(id, _)| *id == workspace)
            .map(|&(_, t)| t)
    }

    /// Keep the *opened at* stamp for `workspace` fresh **without touching it
    /// every time somebody asks**. Returns whether it moved.
    ///
    /// **This is a rule about how often, and it is load-bearing.** The caller is
    /// the idle tick, which runs several times a second forever because the
    /// caret blinks. Stamping milliseconds there changed `Settings` on every
    /// one, so the "only save when something moved" guard above it was never
    /// once true and three configuration files were rewritten at blink rate —
    /// which is what `maintenance.persist_settings` was stalling on, for
    /// seconds at a time, in a profile folder that may be synced.
    ///
    /// Minutes are the unit of the question it answers ("was this workspace
    /// looked at recently"), and every real switch stamps exactly, which is
    /// what orders the recents.
    pub fn refresh_opened_in(&mut self, workspace: WorkspaceId, now: i64) -> bool {
        let last = self.opened_in(workspace).unwrap_or(0);
        if now - last < OPENED_REFRESH_MS {
            return false;
        }
        self.set_opened_in(workspace, now);
        true
    }

    /// Record that `workspace` was opened now.
    pub fn set_opened_in(&mut self, workspace: WorkspaceId, at: i64) {
        match self.opened.iter_mut().find(|(id, _)| *id == workspace) {
            Some(slot) => slot.1 = at,
            None => self.opened.push((workspace, at)),
        }
    }

    /// How wide the pane was in `workspace`, if it was ever set.
    pub fn sidebar_w_in(&self, workspace: WorkspaceId) -> Option<f32> {
        self.sidebar_w
            .iter()
            .find(|(id, _)| *id == workspace)
            .map(|&(_, w)| w)
    }

    /// Record the pane's width in `workspace`, leaving every other alone — the
    /// same rule `set_collapsed_in` exists for.
    pub fn set_sidebar_w_in(&mut self, workspace: WorkspaceId, w: f32) {
        match self.sidebar_w.iter_mut().find(|(id, _)| *id == workspace) {
            Some(slot) => slot.1 = w,
            None => self.sidebar_w.push((workspace, w)),
        }
    }

    /// The profile's own file — the window and the session, and nothing else.
    ///
    /// **Preferences are not read here any more.** Every value the settings
    /// registry owns arrives from the authority through
    /// [`crate::preferences::SettingsLink`]; what this returns carries the
    /// registry's defaults until the authoritative section lands, which is what
    /// lets a window draw before anything has been asked.
    ///
    /// Geometry is **not** applied here either: it is keyed by profile id,
    /// which is not known until the profile has been opened. See
    /// [`apply_window_state`].
    ///
    /// [`apply_window_state`]: Self::apply_window_state
    pub fn load(profile_dir: &Path, peer: &str) -> Self {
        let mut s = Self::from_profile_file(profile_dir, peer);
        migrate_relocated_keys(&mut s);
        s
    }

    /// Put this machine's window geometry over the top.
    ///
    /// Two steps rather than one because the key is the profile's id, which
    /// `load` runs too early to know — and there is room for it: `App::new`
    /// finishes long before a window is created.
    pub fn apply_window_state(
        &mut self,
        profile: koru_core::ProfileId,
        slot: &str,
        root: Option<&Path>,
    ) {
        let want = window_key(profile, slot);
        let Some(row) = load_window_row(root, &want) else {
            return;
        };
        self.apply_window_row(&row);
    }

    /// Honour `--window-size` after remembered geometry has been applied.
    ///
    /// A size on the command line is a request for *this* launch, so it wins
    /// over the last session's size and over a maximised restore: maximising
    /// would throw the requested size away. Position is left alone.
    pub(crate) fn apply_launch_window_size(&mut self, size: (f32, f32)) {
        self.size = size;
        self.maximized = false;
    }

    /// Apply one `windows.conf` value. Split out so corrupt and minimised rows
    /// are testable without touching the process-global LocalAppData file.
    fn apply_window_row(&mut self, row: &str) {
        let mut x = None;
        let mut y = None;
        let mut fx = None;
        let mut fy = None;
        for (key, value) in key_values(&row.replace(
            ';', "
",
        )) {
            match key {
                "x" => x = value.parse::<i32>().ok(),
                "y" => y = value.parse::<i32>().ok(),
                "fx" => fx = value.parse::<i32>().ok(),
                "fy" => fy = value.parse::<i32>().ok(),
                "w" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if (MIN_WINDOW_SIZE.0..=20_000.0).contains(&v) {
                            self.size.0 = v;
                        }
                    }
                }
                "h" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if (MIN_WINDOW_SIZE.1..=20_000.0).contains(&v) {
                            self.size.1 = v;
                        }
                    }
                }
                "maximized" => self.maximized = value == "1",
                "pinned" => self.pinned = value == "1",
                _ => {}
            }
        }
        if let (Some(fx), Some(fy)) = (fx, fy) {
            self.floating_pos = Some((fx, fy));
        }
        self.pos = match (x, y) {
            (Some(x), Some(y)) if usable_window_position(x, y) => Some((x, y)),
            _ => None,
        };
    }

    fn from_profile_file(profile_dir: &Path, peer: &str) -> Self {
        fs::read_to_string(config_path(profile_dir, peer))
            .map_or_else(|_| Self::default(), |text| Self::from_text(&text))
    }

    /// Parse the file body. Split out so the minimised-sentinel and the guards
    /// against a bad write are testable without touching disk.
    fn from_text(text: &str) -> Self {
        let mut s = Self::default();
        let mut x = None;
        let mut y = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "x" => x = value.parse::<i32>().ok(),
                "y" => y = value.parse::<i32>().ok(),
                // Guard against a zero or absurd size from a bad write.
                "w" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if (MIN_WINDOW_SIZE.0..=20_000.0).contains(&v) {
                            s.size.0 = v;
                        }
                    }
                }
                "h" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if (MIN_WINDOW_SIZE.1..=20_000.0).contains(&v) {
                            s.size.1 = v;
                        }
                    }
                }
                // Authority-owned preference keys (theme, fonts, zoom, …) no
                // longer belong here. Legacy copies fall through to `_`; the
                // authority reads and migrates its own file.
                "profile_mru" => {
                    s.profile_mru = value
                        .split(',')
                        .filter_map(koru_core::ProfileId::parse)
                        .collect();
                }
                "terminal_looks" => s.terminal_looks = read_looks(value),
                "hide_completed" => s.hide_completed = value == "1" || value == "true",
                "pinned" => s.pinned = value == "1" || value == "true",
                "maximized" => s.maximized = value == "1" || value == "true",
                "sidebar" => s.sidebar_open = value == "1" || value == "true",
                // The legacy two-state key, kept readable for one release. It
                // loses to `navigator_mode` whichever order the two arrive in,
                // because a build that wrote both wrote this one as an
                // approximation of that one.
                "navigator_files" => {
                    if s.navigator_mode.is_empty() && (value == "1" || value == "true") {
                        s.navigator_mode = "files".to_owned();
                    }
                }
                "navigator_mode" => s.navigator_mode = value.to_owned(),
                // Unparseable is `None`, which reopens whatever the profile
                // lists first — the same tolerance every key here has.
                "workspace" => s.workspace = WorkspaceId::parse(value),
                "context" => s.context_open = value == "1" || value == "true",
                "ai" => s.ai_open = value == "1" || value == "true",
                "ai_w" => {
                    if let Ok(w) = value.parse::<f32>()
                        && (400.0..=900.0).contains(&w)
                    {
                        s.ai_w = w;
                    }
                }
                // `welcome`, `workspace` or a uuid, each optionally `:<scroll>`.
                // An entry that will not parse is skipped on its own — losing
                // one tab is survivable, losing the session is not.
                "tabs" => s.tabs = parse_tabs(value),
                "tab" => s.tab = value.parse::<usize>().unwrap_or(0),
                "pane_tree" => s.pane_tree = PaneTreeState::parse(value),
                "pane_focused" => {
                    s.pane_tree = s.pane_tree.map(|mut pt| {
                        pt.focused = value
                            .split('/')
                            .filter_map(|e| e.trim().parse::<u8>().ok())
                            .collect();
                        pt
                    });
                }
                "pinned_tabs" => {
                    s.pinned_tabs = value
                        .split(',')
                        .filter(|e| !e.trim().is_empty())
                        .filter_map(PinnedTabRef::parse)
                        .collect();
                }
                "pinned_tab" => s.pinned_tab = value.parse::<usize>().ok(),
                // `session.<workspace id>=<active index>;<entries>`, one line
                // per workspace, keyed for the reason `collapsed.` is: a tab
                // names a document, and a document belongs to one workspace.
                // `tabs`/`tab` above are the *open* workspace's copy of its row.
                k if k.starts_with(SESSION_PREFIX) => {
                    if let Some(id) = WorkspaceId::parse(&k[SESSION_PREFIX.len()..]) {
                        let (active, entries) = value.split_once(';').unwrap_or(("0", value));
                        s.sessions.push((
                            id,
                            parse_tabs(entries),
                            active.trim().parse::<usize>().unwrap_or(0),
                        ));
                    }
                }
                "recents" => {
                    s.recents = value
                        .split(',')
                        .filter_map(|e| DocumentId::parse(e.trim()))
                        .take(MAX_RECENTS)
                        .collect();
                }
                // `collapsed.<workspace id>`, one line per workspace. A line
                // whose key does not parse is dropped rather than folded into
                // somebody else's set: the ids in it are meaningless anywhere
                // but the workspace they came from.
                k if k.starts_with(COLLAPSED_PREFIX) => {
                    if let Some(id) = WorkspaceId::parse(&k[COLLAPSED_PREFIX.len()..]) {
                        let ids: Vec<NodeId> = value
                            .split(',')
                            .filter_map(|e| NodeId::parse(e.trim()))
                            .collect();
                        s.collapsed.push((id, ids));
                    }
                }
                // Per-document view-mode / page-colour overrides, one line per
                // workspace: `docpref.<ws>=<node>|<mode>|<bg>,<node>|...`. An
                // entry that will not parse is dropped on its own, the same
                // tolerance every other list here has.
                k if k.starts_with(DOCPREF_PREFIX) => {
                    if let Some(id) = WorkspaceId::parse(&k[DOCPREF_PREFIX.len()..]) {
                        let entries: Vec<(NodeId, DocViewPrefs)> =
                            value.split(',').filter_map(parse_doc_pref_entry).collect();
                        if !entries.is_empty() {
                            s.doc_view_prefs.push((id, entries));
                        }
                    }
                }
                k if k.starts_with(OPENED_PREFIX) => {
                    if let Some(id) = WorkspaceId::parse(&k[OPENED_PREFIX.len()..])
                        && let Ok(t) = value.parse::<i64>()
                    {
                        s.opened.push((id, t));
                    }
                }
                k if k.starts_with(SIDEBAR_W_PREFIX) => {
                    if let Some(id) = WorkspaceId::parse(&k[SIDEBAR_W_PREFIX.len()..])
                        && let Ok(w) = value.parse::<f32>()
                        && SIDEBAR_W_RANGE.contains(&w)
                    {
                        s.sidebar_w.push((id, w));
                    }
                }
                "show_archived" => s.show_archived = value == "true",
                // **Relocated here from `preferences.conf` (ADR-266).** Neither
                // is a preference: one is a usage ranking the picker derives,
                // the other is where a file dialog last was. They belong to the
                // session this host owns, and their old home now has exactly
                // one reader and one writer — the settings authority.
                "emoji_recents" => {
                    s.emoji_recents = value
                        .split(',')
                        .filter(|item| !item.is_empty())
                        .take(MAX_EMOJI_RECENTS)
                        .map(str::to_owned)
                        .collect();
                }
                "last_export_dir" => {
                    s.last_export_dir = (!value.is_empty()).then(|| PathBuf::from(value));
                }
                // Read whole and untouched: its reader is the Chats list's.
                "chat_seen" => s.chat_seen = value.to_owned(),
                // Read whole and untouched too: its reader is the rail's.
                "rail_order" => s.rail_order = value.to_owned(),
                _ => {}
            }
        }
        // The active index is written by one run and read by another that may
        // have dropped a tab in between, so it is clamped rather than trusted.
        if s.tab >= s.tabs.len() {
            s.tab = s.tabs.len().saturating_sub(1);
        }
        // Fill missing home workspaces on legacy `doc:<uuid>` Main entries from
        // the window's current workspace — enough to reopen without a locate.
        if let Some(ws) = s.workspace {
            for (which, _) in &mut s.tabs {
                if let TabRef::Document {
                    workspace: home @ None,
                    ..
                } = which
                {
                    *home = Some(ws);
                }
            }
        }
        if let (Some(x), Some(y)) = (x, y) {
            // Windows reports (-32000, -32000) for a minimised window, and an
            // older build could write that to disk. Restoring it would open the
            // window off-screen, so drop it and let the OS place the window
            // instead. No real monitor layout reaches both axes below -10_000.
            if usable_window_position(x, y) {
                s.pos = Some((x, y));
            }
        }
        s
    }

    /// Best-effort: a failure to save the session is not worth surfacing.
    ///
    /// Writes the profile file now and queues this process's one geometry row
    /// to the shared off-thread writer. **Preferences are not here**: they are
    /// patched to the authority by [`crate::preferences::SettingsLink`], which
    /// is what stops two windows saving at once from losing one another's.
    pub fn save_all(&self, profile_dir: &Path, peer: &str, written: &mut Written) {
        let body = self.profile_body();
        self.write_profile(profile_dir, peer, &body);
        written.profile = Some(body);

        written.window.write_if_changed(self.window_row());
    }

    /// The same two files, but each written only when **its own** contents
    /// changed.
    ///
    /// **"Did any setting move" is the wrong question, and asking it cost a
    /// write a tick.** These are files about different things: scrolling a tab
    /// rewrote the window row, and a clock stamp nothing reads to the second
    /// rewrote both. Comparing the rendered body against what was last written
    /// asks what a file write is actually for, and it cannot be defeated by a
    /// new field the way an `==` on the struct was.
    ///
    /// [`Written`] is the caller's memory of this process's own writes, so a
    /// first save after launch always happens — which is what makes it correct
    /// rather than merely cheap when another window owns a row in the same file.
    pub fn save_all_changed(&self, profile_dir: &Path, peer: &str, written: &mut Written) {
        let body = self.profile_body();
        // The row this window owns, which is the only part of the geometry file
        // it is entitled to change — so it is also the only part worth
        // comparing. The shared row writer performs the read-merge-write on
        // its worker thread so several profile windows retain one another's
        // rows without making this thread touch the geometry file.
        let row = self.window_row();
        let changes = written.changes(&body, &row);
        if changes.profile {
            self.write_profile(profile_dir, peer, &body);
            written.profile = Some(body);
        }
        if changes.window_row {
            written.window.write_if_changed(row);
        }
    }

    /// This window's row in the geometry file — the whole of what it owns
    /// there.
    ///
    /// Split out so [`save_all_changed`](Self::save_all_changed) can ask
    /// whether it moved without reading the file it would otherwise rewrite.
    fn window_row(&self) -> String {
        let (x, y) = self.pos.unwrap_or((0, 0));
        let mut row = format!(
            "x={x};y={y};w={:.0};h={:.0};maximized={};pinned={}",
            self.size.0,
            self.size.1,
            u8::from(self.maximized),
            u8::from(self.pinned),
        );
        if let Some((fx, fy)) = self.floating_pos {
            row.push_str(&format!(";fx={fx};fy={fy}"));
        }
        row
    }

    /// The write itself, with the body already in hand — so a caller that has
    /// rendered it to compare against the last one does not render it twice.
    fn write_profile(&self, profile_dir: &Path, peer: &str, body: &str) {
        let path = config_path(profile_dir, peer);
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&path, body);
    }

    /// The profile file's contents.
    ///
    /// **Split out so the round-trip test can assert against `save`'s own
    /// formatting rather than against a copy of it.** It was a copy, briefly,
    /// and the copy grew a `collapsed=` line that `save` never had — so the test
    /// passed while the feature quietly did not persist. One function is the fix.
    ///
    /// **It no longer writes what moved out.** Preferences and geometry live in
    /// their own files now; leaving stale copies here would mean a hand-edited
    /// a hand-edited preference key in a profile folder that silently does
    /// nothing. `from_text`
    /// still *reads* them, which is the whole of the promotion path: on the
    /// first run after the split the old keys are still here, are read, and are
    /// then written to their new homes and dropped from this one.
    fn profile_body(&self) -> String {
        let mut out = String::from(
            "# What Koru had open in this profile. Preferences and window\n\
             # geometry are elsewhere - see docs/contributing/operations/RUNBOOK.md.\n",
        );
        out.push_str(&format!(
            "hide_completed={}\nsidebar={}\nnavigator_files={}\nnavigator_mode={}\ncontext={}\nai={}\nai_w={}\n",
            u8::from(self.hide_completed),
            u8::from(self.sidebar_open),
            // **Both keys, deliberately.** The token is what this build reads;
            // the bool is what the last one reads, and writing an approximation
            // of the token into it is what stops a session round-tripping
            // through an older build losing the Files projection.
            u8::from(self.navigator_mode == "files"),
            if self.navigator_mode.is_empty() {
                "documents"
            } else {
                self.navigator_mode.as_str()
            },
            u8::from(self.context_open),
            u8::from(self.ai_open),
            self.ai_w,
        ));
        if !self.profile_mru.is_empty() {
            out.push_str("profile_mru=");
            out.push_str(
                &self
                    .profile_mru
                    .iter()
                    .map(|profile| profile.to_storage())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
        let looks = write_looks(&self.terminal_looks);
        if !looks.is_empty() {
            out.push_str(&format!("terminal_looks={looks}\n"));
        }
        if let Some(id) = self.workspace {
            out.push_str(&format!("workspace={}\n", id.to_storage()));
        }
        out.push_str(&format!(
            "tabs={}\ntab={}\n",
            write_tabs(&self.tabs),
            self.tab
        ));
        if let Some(pt) = &self.pane_tree {
            out.push_str(&format!("pane_tree={}\n", pt.to_storage()));
            let focused = pt
                .focused
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("/");
            out.push_str(&format!("pane_focused={focused}\n"));
        }
        if !self.pinned_tabs.is_empty() {
            let pins: Vec<String> = self.pinned_tabs.iter().map(|p| p.to_storage()).collect();
            out.push_str(&format!("pinned_tabs={}\n", pins.join(",")));
        }
        if let Some(i) = self.pinned_tab {
            out.push_str(&format!("pinned_tab={i}\n"));
        }
        // Legacy `session.<ws>=` lines are still *read* on load but no longer
        // written: the Main strip is window-scoped in `tabs=`.
        let recents: Vec<String> = self.recents.iter().map(DocumentId::to_storage).collect();
        out.push_str(&format!("recents={}\n", recents.join(",")));
        // The two that came *in* from `preferences.conf` (ADR-266). Written
        // only when they hold something, so a fresh session file does not grow
        // two empty keys that mean nothing.
        if !self.emoji_recents.is_empty() {
            out.push_str(&format!("emoji_recents={}\n", self.emoji_recents.join(",")));
        }
        if let Some(directory) = &self.last_export_dir {
            out.push_str(&format!("last_export_dir={}\n", directory.display()));
        }
        if !self.chat_seen.is_empty() {
            out.push_str(&format!("chat_seen={}\n", self.chat_seen));
        }
        if !self.rail_order.is_empty() {
            out.push_str(&format!("rail_order={}\n", self.rail_order));
        }
        // One line per workspace, and none at all for a workspace with nothing
        // shut — so the file does not grow a row for every workspace you have
        // ever glanced at.
        for (workspace, nodes) in &self.collapsed {
            if nodes.is_empty() {
                continue;
            }
            let ids: Vec<String> = nodes.iter().map(NodeId::to_storage).collect();
            out.push_str(&format!(
                "{COLLAPSED_PREFIX}{}={}\n",
                workspace.to_storage(),
                ids.join(",")
            ));
        }
        if self.show_archived {
            out.push_str(
                "show_archived=true
",
            );
        }
        // When each workspace was last opened here.
        for (workspace, at) in &self.opened {
            out.push_str(&format!(
                "{OPENED_PREFIX}{}={at}
",
                workspace.to_storage()
            ));
        }
        // The pane's width, one line per workspace that has moved it.
        for (workspace, w) in &self.sidebar_w {
            out.push_str(&format!(
                "{SIDEBAR_W_PREFIX}{}={}
",
                workspace.to_storage(),
                w.round()
            ));
        }
        // Per-document view-mode / page-colour overrides, one line per workspace.
        // A document following the defaults has no entry, so the file does not
        // grow a row for every document ever opened.
        for (workspace, entries) in &self.doc_view_prefs {
            let kept: Vec<&(NodeId, DocViewPrefs)> =
                entries.iter().filter(|(_, p)| !p.is_empty()).collect();
            if kept.is_empty() {
                continue;
            }
            let parts: Vec<String> = kept
                .iter()
                .map(|(n, p)| {
                    format!(
                        "{}|{}|{}",
                        n.to_storage(),
                        view_mode_token_opt(p.mode),
                        canvas_bg_token_opt(p.canvas_bg)
                    )
                })
                .collect();
            out.push_str(&format!(
                "{DOCPREF_PREFIX}{}={}\n",
                workspace.to_storage(),
                parts.join(",")
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Manage tab is `manage`, beside the legacy `workspaces` and
    /// `profiles` words, which keep their own meanings.
    #[test]
    fn the_manage_tab_round_trips_beside_the_legacy_words() {
        assert_eq!(TabRef::Manage.to_storage(), "manage");
        let tabs = parse_tabs("manage:0,workspaces:0,profiles:0");
        assert_eq!(
            tabs.into_iter().map(|(which, _)| which).collect::<Vec<_>>(),
            vec![TabRef::Manage, TabRef::Workspaces, TabRef::ManageProfiles]
        );
    }

    /// The Sync settings page has its own word; an older reader drops it
    /// and keeps its neighbours, as it does for every word it does not know.
    #[test]
    fn the_sync_settings_tab_round_trips_and_an_older_reader_skips_it() {
        assert_eq!(TabRef::SyncSettings.to_storage(), "sync-settings");
        let tabs = parse_tabs("welcome:0,sync-settings:0,manage:0");
        assert_eq!(
            tabs.into_iter().map(|(which, _)| which).collect::<Vec<_>>(),
            vec![TabRef::Welcome, TabRef::SyncSettings, TabRef::Manage]
        );
        assert_eq!(TabRef::parse("sync-settings"), Some(TabRef::SyncSettings));
    }

    /// The Reminders page is one `tasks:` spelling like any other — and an
    /// older build, which knows neither the `list:` prefix nor a bucket by
    /// that name, drops just that entry and keeps its neighbours.
    #[test]
    fn the_reminders_tab_round_trips_and_reads_forgivingly() {
        let stored = TasksTabRef::Reminders.to_storage();
        assert_eq!(stored, "tasks:reminders");
        assert_eq!(
            TasksTabRef::parse("reminders"),
            Some(TasksTabRef::Reminders)
        );

        let tabs = parse_tabs("tasks:today:0,tasks:reminders:0,trash:0");
        assert_eq!(
            tabs.into_iter().map(|(which, _)| which).collect::<Vec<_>>(),
            vec![
                TabRef::Tasks(TasksTabRef::Bucket("today")),
                TabRef::Tasks(TasksTabRef::Reminders),
                TabRef::Trash,
            ]
        );
        // The older reader's answer, simulated: the token is no bucket and no
        // `list:` id, so the entry — and only the entry — is skipped.
        assert_eq!(TasksTabRef::parse("something-else"), None);
    }

    #[test]
    fn pre_identity_settings_need_no_profile() {
        let remembered = WorkspaceId::new();
        let mut values = Settings::from_text(&format!(
            "workspace={}\ntabs=welcome:12\n",
            remembered.to_storage()
        ));
        values.theme = ThemePreference::Dark;
        let staged = PreIdentitySettings::from_settings(values);

        assert_eq!(staged.workspace_hint(), Some(remembered));
        assert_eq!(staged.preferences().theme, ThemePreference::Dark);
    }

    #[test]
    fn profile_keyed_window_state_applies_only_when_bound_to_a_real_profile() {
        let staged = PreIdentitySettings::from_settings(Settings::default());
        let profile = koru_core::ProfileId::new();
        let bound =
            staged.bind_with_window_row(profile, Some("x=120\ny=80\nw=1000\nh=700\nmaximized=1\n"));

        assert_eq!(bound.profile_id(), profile);
        assert_eq!(bound.pos, Some((120, 80)));
        assert_eq!(bound.size, (1000.0, 700.0));
        assert!(bound.maximized);
    }

    #[test]
    fn a_launch_window_size_wins_over_remembered_geometry_and_maximise() {
        let mut settings = Settings::default();
        settings.apply_window_row("x=120;y=80;w=1000;h=700;maximized=1");
        settings.apply_launch_window_size((1024.0, 768.0));

        assert_eq!(settings.size, (1024.0, 768.0));
        assert!(!settings.maximized);
        assert_eq!(settings.pos, Some((120, 80)));
    }

    /// The reported bug, end to end at the policy seam: a position remembered
    /// on a monitor that has since been unplugged is a hint about a desktop
    /// that no longer exists, and restoring it verbatim opened the window
    /// off-screen with nothing to hold. The live monitor set gets a say.
    #[test]
    fn a_remembered_position_is_only_restored_where_the_desktop_still_has_it() {
        let screen = (0, 0, 1920, 1080);

        // On the one screen that exists: honoured as before.
        assert_eq!(
            restorable_position(Some((120, 80)), (940.0, 620.0), 1.0, &[screen]),
            Some((120, 80))
        );
        // Remembered on a second monitor left of the primary, now unplugged.
        assert_eq!(
            restorable_position(Some((-1920, 0)), (940.0, 620.0), 1.0, &[screen]),
            None
        );
        // Same monitor still connected: fine, negative coordinates and all.
        assert_eq!(
            restorable_position(
                Some((-1920, 0)),
                (940.0, 620.0),
                1.0,
                &[screen, (-1920, 0, 0, 1080)]
            ),
            Some((-1920, 0))
        );
        // The minimised sentinel never reaches the monitor question.
        assert_eq!(
            restorable_position(Some((-32000, -32000)), (940.0, 620.0), 1.0, &[screen]),
            None
        );
        // Nothing remembered is nothing restored.
        assert_eq!(
            restorable_position(None, (940.0, 620.0), 1.0, &[screen]),
            None
        );
    }

    /// The remembered size is logical and the monitor question is physical, so
    /// the primary scale is applied before asking — a 940x620 hint at 150% is
    /// a 1410x930 rectangle, and the wrong unit would test a different window
    /// than the one that opens: at x=-900 the physical window reaches 510px
    /// onto the screen (a grip), the logical one only 40 (a seam).
    #[test]
    fn restorable_position_scales_the_remembered_size_before_asking() {
        let screen = (0, 0, 1920, 1080);
        assert_eq!(
            restorable_position(Some((-900, 0)), (940.0, 620.0), 1.5, &[screen]),
            Some((-900, 0)),
            "the physical window is 1410 wide and gets a grip on the screen"
        );
        assert_eq!(
            restorable_position(Some((-900, 0)), (940.0, 620.0), 1.0, &[screen]),
            None,
            "at 100% only 40px of width reaches the screen — not a grip"
        );
    }

    // Dictation preference storage and migration are tested in `koru-settings`;
    // this crate tests only the authority-to-window conversion in `preferences`.

    /// **An idle tick must not stamp the clock.**
    ///
    /// This is the bug the whole `refresh_opened_in` rule exists for: the idle
    /// loop runs several times a second forever, and a millisecond stamp there
    /// changed `Settings` on every one — so the save guard above it was never
    /// true and three configuration files were rewritten at caret-blink rate.
    /// Measured in a running window as `maintenance.persist_settings` blocking
    /// the main thread for up to 7.9 seconds.
    #[test]
    fn refreshing_the_opened_stamp_is_a_no_op_until_it_is_stale() {
        let ws = WorkspaceId::new();
        let mut s = Settings::default();

        assert!(s.refresh_opened_in(ws, 1_000_000), "never stamped before");
        assert!(!s.refresh_opened_in(ws, 1_000_001), "one millisecond later");
        assert!(!s.refresh_opened_in(ws, 1_000_000 + OPENED_REFRESH_MS - 1));
        assert!(s.refresh_opened_in(ws, 1_000_000 + OPENED_REFRESH_MS));
        assert_eq!(s.opened_in(ws), Some(1_000_000 + OPENED_REFRESH_MS));
    }

    /// A real switch still stamps exactly, which is what orders the recents —
    /// the coarse refresh above is a keep-alive, not the record.
    #[test]
    fn an_explicit_stamp_is_never_coarsened() {
        let ws = WorkspaceId::new();
        let mut s = Settings::default();
        s.set_opened_in(ws, 1_000_000);
        s.set_opened_in(ws, 1_000_001);
        assert_eq!(s.opened_in(ws), Some(1_000_001));
    }

    /// **Two files, two questions.** Any field moving used to rewrite every
    /// file, so scrolling a tab rewrote the font preferences. Comparing each
    /// file's own rendered body is what makes a save mean something — and the
    /// preferences are not one of these files any more: they are a patch to
    /// the authority, which names the fields it changes rather than a body.
    #[test]
    fn only_the_file_whose_contents_moved_is_written() {
        let mut written = Written::default();

        // Nothing written yet: the first save always happens, whatever is on
        // disk — another window may own a row in the geometry file.
        let all = written.changes("profile", "row");
        assert_eq!(
            all,
            Changes {
                profile: true,
                window_row: true
            }
        );

        written.profile = Some("profile".into());
        written.window.write_if_changed("row");
        assert!(
            !written.changes("profile", "row").any(),
            "an idle tick that moved nothing writes nothing"
        );

        assert_eq!(
            written.changes("profile", "moved"),
            Changes {
                profile: false,
                window_row: true
            },
            "moving the window must not rewrite the session"
        );
    }

    /// The pane's width is per workspace, so switching and coming back gives
    /// each the width it was left at rather than one shared number.
    #[test]
    fn the_pane_width_round_trips_per_workspace() {
        let a = WorkspaceId::new();
        let b = WorkspaceId::new();
        let mut s = Settings::default();
        s.set_sidebar_w_in(a, 420.0);
        s.set_sidebar_w_in(b, 232.0);

        let back = Settings::from_text(&s.profile_body());

        assert_eq!(back.sidebar_w_in(a), Some(420.0));
        assert_eq!(back.sidebar_w_in(b), Some(232.0));
        assert_eq!(back.sidebar_w_in(WorkspaceId::new()), None);
    }

    /// Setting one workspace's width leaves every other alone. Writing the field
    /// directly is what discarded the workspace you had just left, and this list
    /// has the same hazard the collapsed one does.
    #[test]
    fn setting_one_workspaces_width_does_not_disturb_another() {
        let a = WorkspaceId::new();
        let b = WorkspaceId::new();
        let mut s = Settings::default();
        s.set_sidebar_w_in(a, 300.0);
        s.set_sidebar_w_in(b, 500.0);
        s.set_sidebar_w_in(a, 350.0);

        assert_eq!(s.sidebar_w_in(a), Some(350.0));
        assert_eq!(s.sidebar_w_in(b), Some(500.0));
    }

    /// A corrupt or hand-edited width is ignored rather than making the window
    /// unusable — the same stance `w`/`h` and `zoom` take.
    #[test]
    fn an_absurd_pane_width_is_ignored() {
        let id = WorkspaceId::new();
        for w in ["0", "-400", "99999", "wide"] {
            let text = format!(
                "sidebar_w.{}={w}
",
                id.to_storage()
            );
            assert_eq!(Settings::from_text(&text).sidebar_w_in(id), None, "{w}");
        }
    }

    /// Pruning takes **every** per-workspace list, not only the collapsed one —
    /// otherwise each grows a row for every workspace the profile has ever had.
    #[test]
    fn pruning_drops_the_pane_width_of_a_workspace_that_is_gone() {
        let live = WorkspaceId::new();
        let gone = WorkspaceId::new();
        let mut s = Settings::default();
        s.set_sidebar_w_in(live, 300.0);
        s.set_sidebar_w_in(gone, 500.0);

        s.prune_collapsed(&[live]);

        assert_eq!(s.sidebar_w_in(live), Some(300.0));
        assert_eq!(s.sidebar_w_in(gone), None);
    }

    #[test]
    fn defaults_are_sane() {
        let d = Settings::default();
        assert_eq!(d.pos, None);
        assert_eq!(d.size, (940.0, 620.0));
        assert!(!d.hide_completed);
        assert_eq!(d.zoom, 1.0);
    }

    /// **The session file has no opinion about a preference.** A `zoom=` line
    /// left in an old profile file is a value with no effect, and reading it
    /// would be a second answer to a question the settings authority owns — the
    /// bounds themselves are `koru_settings::Zoom`'s, tested there.
    #[test]
    fn a_preference_key_left_in_an_old_session_file_is_ignored() {
        for line in ["zoom=1.5\n", "theme=dark\n", "density=compact\n"] {
            let s = Settings::from_text(line);
            assert_eq!(s, Settings::default(), "{line}");
        }
    }

    /// A normal position round-trips through the file body.
    #[test]
    fn a_position_is_read_back() {
        let s = Settings::from_text("x=120\ny=80\nw=1000\nh=700\n");
        assert_eq!(s.pos, Some((120, 80)));
        assert_eq!(s.size, (1000.0, 700.0));
    }

    /// The minimised sentinel (-32000, -32000 on Windows) must not be restored,
    /// or the window opens off-screen and looks dead. This is the bug that made
    /// a peer closed while minimised never come back.
    #[test]
    fn a_minimised_position_is_rejected() {
        let s = Settings::from_text("x=-32000\ny=-32000\nw=747\nh=823\n");
        assert_eq!(s.pos, None, "the sentinel is dropped");
        assert_eq!(s.size, (747.0, 823.0), "size is still kept");
    }

    /// Machine-local geometry has its own parser. It must enforce the same
    /// sentinel and size guards as the legacy profile-file path, or a window
    /// closed while minimised becomes a 158x26 taskbar ghost on every launch.
    #[test]
    fn a_minimised_windows_file_row_is_rejected() {
        let mut s = Settings::default();
        s.apply_window_row("x=-32000;y=-32000;w=158;h=26;maximized=0;pinned=1");

        assert_eq!(s.pos, None);
        assert_eq!(s.size, DEFAULT_WINDOW_SIZE);
        assert!(!s.maximized);
        assert!(s.pinned, "non-geometry state still applies");
    }

    #[test]
    fn explicitly_rooted_workspace_geometry_reads_only_that_root() {
        let parent = tempfile::tempdir().unwrap();
        let root_a = parent.path().join("a");
        let root_b = parent.path().join("b");
        let profile = koru_core::ProfileId::new();
        let key = window_key(profile, "default");
        for (root, x) in [(&root_a, 111), (&root_b, 222)] {
            let path = windows_path(Some(root)).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!("{key} = x={x};y=40;w=900;h=600;maximized=0;pinned=0\n"),
            )
            .unwrap();
        }

        let mut a = Settings::default();
        a.apply_window_state(profile, "default", Some(&root_a));
        let mut b = Settings::default();
        b.apply_window_state(profile, "default", Some(&root_b));
        assert_eq!(a.pos, Some((111, 40)));
        assert_eq!(b.pos, Some((222, 40)));
    }

    /// A genuinely negative coordinate — a monitor to the left or above the
    /// primary — is fine; only the sentinel is dropped.
    #[test]
    fn a_negative_position_is_kept() {
        let s = Settings::from_text("x=-1500\ny=-900\nw=940\nh=620\n");
        assert_eq!(s.pos, Some((-1500, -900)));
    }

    // ------------------------------------------------------------- the session

    /// Everything the container remembers about *this profile* survives a write
    /// and a read. The round-trip is through `save`'s own body, so a field added
    /// to one and not the other fails here rather than silently vanishing on
    /// restart — which is exactly what happened when this test had a copy of the
    /// formatting instead of the real thing.
    ///
    /// Preferences and geometry are deliberately absent: they are two other
    /// files, and their round-trips are their own.
    #[test]
    fn the_whole_session_round_trips() {
        let a = NodeId::new();
        let b = NodeId::new();
        let ws = WorkspaceId::new();
        let recents = vec![DocumentId::new(), DocumentId::new()];
        let mut before = Settings {
            workspace: Some(ws),
            hide_completed: true,
            sidebar_open: true,
            navigator_mode: "tasks".to_owned(),
            context_open: false,
            ai_open: true,
            ai_w: 712.0,
            tabs: vec![
                (TabRef::Welcome, 0.0),
                (TabRef::Node(a), 120.0),
                (TabRef::Welcome, 0.0),
            ],
            tab: 1,
            recents,
            collapsed: vec![(WorkspaceId::new(), vec![a, b])],
            ..Settings::default()
        };
        // The open workspace's row, which the shell files on every capture —
        // spelled here rather than left out, so the comparison is against a
        // saved session rather than against a half-built one.
        before.remember_tabs_of(ws);
        let after = Settings::from_text(&before.profile_body());
        assert_eq!(after, before);
    }

    /// A split-view pane tree survives a write and a read: nested splits, tab
    /// groups, active indices, and the focused path.
    #[test]
    fn pane_tree_round_trips() {
        let a = NodeId::new();
        let b = NodeId::new();
        let doc = DocumentId::new();
        let ws = WorkspaceId::new();
        let tree = PaneTreeState {
            root: PaneNodeState::Split {
                axis: SplitAxisState::Vertical,
                ratio: 0.333,
                left: Box::new(PaneNodeState::Group {
                    main: vec![(TabRef::Welcome, 0.0), (TabRef::Node(a), 12.0)],
                    pinned: vec![PinnedTabRef::document_pin(doc, ws, 7.0, true)],
                    active: GroupActiveState::Pinned(0),
                }),
                right: Box::new(PaneNodeState::Group {
                    main: vec![(TabRef::Node(b), 24.0)],
                    pinned: vec![],
                    active: GroupActiveState::Main(0),
                }),
            },
            focused: vec![1],
        };
        let mut before = Settings {
            workspace: Some(ws),
            pane_tree: Some(tree),
            ..Settings::default()
        };
        before.remember_tabs_of(ws);
        let after = Settings::from_text(&before.profile_body());
        assert_eq!(after.pane_tree, before.pane_tree);
    }

    /// Two columns, a document in the left and a new chat (which is not saved
    /// until it is sent into) in the right, with the right focused: `tabs=` is the focused group's copy
    /// and is empty, so the document is only in the tree. It must still be one
    /// the shell reopens before restore — or the column comes back as Welcome,
    /// which is what a restart did.
    #[test]
    fn a_document_in_the_unfocused_column_is_remembered() {
        let ws = WorkspaceId::new();
        let (left, pinned) = (DocumentId::new(), DocumentId::new());
        let text = format!(
            "workspace={ws}\ntabs=\ntab=0\n\
             pane_tree=split(v;0.500;group(doc:{left}@{ws}:0;doc:{pinned}@{ws}:0:icon;m0);group(;;m0))|1\n\
             pane_focused=1\n",
            ws = ws.to_storage(),
            left = left.to_storage(),
            pinned = pinned.to_storage(),
        );
        let s = Settings::from_text(&text);
        assert!(s.tabs.is_empty(), "the focused column held no document");
        assert_eq!(
            s.remembered_documents(),
            vec![(pinned, Some(ws)), (left, Some(ws))]
        );
        assert_eq!(s.open_document_ids(), vec![pinned, left]);
    }

    /// The focused group's documents are in both `tabs` and the tree; each is
    /// reopened once, and a legacy entry with no home takes the tree's.
    #[test]
    fn a_document_in_both_the_flat_list_and_the_tree_is_remembered_once() {
        let ws = WorkspaceId::new();
        let doc = DocumentId::new();
        let s = Settings {
            tabs: vec![(TabRef::document(doc, None), 0.0)],
            pane_tree: Some(PaneTreeState {
                root: PaneNodeState::Group {
                    main: vec![(TabRef::document(doc, Some(ws)), 0.0)],
                    pinned: vec![],
                    active: GroupActiveState::Main(0),
                },
                focused: vec![],
            }),
            ..Settings::default()
        };
        assert_eq!(s.remembered_documents(), vec![(doc, Some(ws))]);
    }

    /// **A chat tab is a word of the file** (ADR-483-chattabrestore):
    /// `chat:<uuid>` in `tabs=`, in a group of `pane_tree=` and, pinned, in
    /// `pinned_tabs=`, each back as it went out — and beside a document whose
    /// scroll and home survive with it.
    #[test]
    fn a_chat_tab_round_trips_in_every_place_a_tab_is_written() {
        let ws = WorkspaceId::new();
        let doc = DocumentId::new();
        let (chat, pinned_chat, column_chat) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        let chat_pin = PinnedTabRef {
            target: PinnedTarget::Chat(pinned_chat),
            workspace: ws,
            scroll: 0.0,
            show_title: true,
        };
        let before = Settings {
            workspace: Some(ws),
            tabs: vec![
                (TabRef::document(doc, Some(ws)), 40.0),
                (TabRef::Chat(chat), 0.0),
            ],
            tab: 1,
            pinned_tabs: vec![chat_pin, PinnedTabRef::document_pin(doc, ws, 0.0, false)],
            pane_tree: Some(PaneTreeState {
                root: PaneNodeState::Split {
                    axis: SplitAxisState::Vertical,
                    ratio: 0.5,
                    left: Box::new(PaneNodeState::Group {
                        main: vec![
                            (TabRef::document(doc, Some(ws)), 40.0),
                            (TabRef::Chat(chat), 0.0),
                        ],
                        pinned: vec![chat_pin],
                        active: GroupActiveState::Main(1),
                    }),
                    right: Box::new(PaneNodeState::Group {
                        main: vec![(TabRef::Chat(column_chat), 0.0)],
                        pinned: vec![],
                        active: GroupActiveState::Main(0),
                    }),
                },
                focused: vec![0],
            }),
            ..Settings::default()
        };
        let body = before.profile_body();
        assert!(
            body.contains(&format!("chat:{chat}")),
            "spelled chat:<uuid>: {body}"
        );
        let after = Settings::from_text(&body);
        assert_eq!(after.tabs, before.tabs);
        assert_eq!(after.tab, 1);
        assert_eq!(after.pinned_tabs, before.pinned_tabs);
        assert_eq!(after.pane_tree, before.pane_tree);
    }

    /// `remembered_documents` is what the shell opens from the store before
    /// restore. A chat is the AI service's, never the store's: no chat entry —
    /// in `tabs`, pinned, or in a column of the tree — names a document.
    #[test]
    fn remembered_documents_ignores_chat_tabs() {
        let ws = WorkspaceId::new();
        let doc = DocumentId::new();
        let chat = uuid::Uuid::new_v4();
        let s = Settings {
            tabs: vec![(TabRef::Chat(chat), 0.0)],
            pinned_tabs: vec![PinnedTabRef {
                target: PinnedTarget::Chat(chat),
                workspace: ws,
                scroll: 0.0,
                show_title: false,
            }],
            pane_tree: Some(PaneTreeState {
                root: PaneNodeState::Group {
                    main: vec![
                        (TabRef::Chat(chat), 0.0),
                        (TabRef::document(doc, Some(ws)), 0.0),
                    ],
                    pinned: vec![PinnedTabRef {
                        target: PinnedTarget::Chat(uuid::Uuid::new_v4()),
                        workspace: ws,
                        scroll: 0.0,
                        show_title: false,
                    }],
                    active: GroupActiveState::Main(0),
                },
                focused: vec![],
            }),
            ..Settings::default()
        };
        assert_eq!(s.remembered_documents(), vec![(doc, Some(ws))]);
    }

    /// A chat entry that is not a uuid is not a chat: dropped on its own,
    /// like any entry that will not parse.
    #[test]
    fn a_chat_entry_that_is_not_a_uuid_is_dropped_alone() {
        let parsed = parse_tabs("welcome:0,chat:not-a-chat:0,trash:0");
        assert_eq!(parsed, vec![(TabRef::Welcome, 0.0), (TabRef::Trash, 0.0)]);
        assert_eq!(PinnedTabRef::parse("chat:nope@x:0:icon"), None);
    }

    /// **What a chat entry means to a build that has never heard of one.**
    /// The grammar before `chat:` — keywords, `tasks:`, `session:`, `doc:`,
    /// then a bare node id — and the pinned grammar that requires `doc:` are
    /// replicated below, reduced to *what kind of tab came back*. A `chat:`
    /// entry falls through all of it, so an older reader drops that one entry
    /// and keeps every neighbour (ADR-483-chattabrestore).
    #[test]
    fn an_older_reader_skips_a_chat_entry_and_keeps_the_other_tabs() {
        fn old_parse(s: &str) -> Option<&'static str> {
            match s {
                "welcome" | "workspaces" | "profiles" | "manage" | "trash" | "whatsnew"
                | "sync-settings" | "draft" => Some("keyword"),
                other if other.starts_with("tasks:") => Some("tasks"),
                other if other.starts_with("session:") => Some("session"),
                other => match other.strip_prefix("doc:") {
                    Some(rest) => {
                        let id = rest.split_once('@').map_or(rest, |(id, _)| id);
                        koru_core::DocumentId::parse(id).map(|_| "document")
                    }
                    None => NodeId::parse(other).map(|_| "node"),
                },
            }
        }
        /// `PinnedTabRef::parse` before pinned chats, reduced the same way.
        fn old_pin(s: &str) -> Option<&'static str> {
            let rest = s.rsplit_once(':').map_or(s, |(rest, _)| rest);
            let rest = rest.rsplit_once(':').map_or(rest, |(rest, _)| rest);
            let (doc, ws) = rest.split_once('@')?;
            DocumentId::parse(doc.strip_prefix("doc:")?)?;
            WorkspaceId::parse(ws)?;
            Some("pin")
        }
        let ws = WorkspaceId::new();
        let line = write_tabs(&[
            (TabRef::Welcome, 0.0),
            (TabRef::Chat(uuid::Uuid::new_v4()), 0.0),
            (TabRef::document(DocumentId::new(), Some(ws)), 40.0),
            (TabRef::Node(NodeId::new()), 8.0),
        ]);
        // The old entry loop, verbatim: split on commas, peel a numeric scroll
        // from the last colon, hand the rest to the old parser.
        let survivors: Vec<&str> = line
            .split(',')
            .filter_map(|entry| {
                let which = match entry.rsplit_once(':') {
                    Some((w, sc)) if sc.parse::<f32>().is_ok() => w,
                    _ => entry,
                };
                old_parse(which)
            })
            .collect();
        assert_eq!(survivors, ["keyword", "document", "node"]);

        let pins = write_pinned_tabs(&[
            PinnedTabRef {
                target: PinnedTarget::Chat(uuid::Uuid::new_v4()),
                workspace: ws,
                scroll: 0.0,
                show_title: true,
            },
            PinnedTabRef::document_pin(DocumentId::new(), ws, 0.0, false),
        ]);
        let pinned: Vec<&str> = pins.split(',').filter_map(old_pin).collect();
        assert_eq!(
            pinned,
            ["pin"],
            "the pinned chat dropped, the document kept"
        );
    }

    /// A ratio that is not a number is a corrupt row, not a geometry: NaN
    /// survives `clamp`, so every pane rect would become NaN and the window
    /// would draw and hit-test nothing. The tree is refused whole and the
    /// legacy tab keys carry the session instead.
    #[test]
    fn a_pane_tree_with_a_non_finite_ratio_is_refused() {
        let group = || PaneNodeState::Group {
            main: vec![(TabRef::Welcome, 0.0)],
            pinned: vec![],
            active: GroupActiveState::Main(0),
        };
        for ratio in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let tree = PaneTreeState {
                root: PaneNodeState::Split {
                    axis: SplitAxisState::Vertical,
                    ratio,
                    left: Box::new(group()),
                    right: Box::new(group()),
                },
                focused: vec![0],
            };
            assert!(
                PaneTreeState::parse(&tree.to_storage()).is_none(),
                "a ratio of {ratio} was accepted"
            );
        }
        let sound = PaneTreeState {
            root: PaneNodeState::Split {
                axis: SplitAxisState::Vertical,
                ratio: 0.5,
                left: Box::new(group()),
                right: Box::new(group()),
            },
            focused: vec![0],
        };
        assert_eq!(PaneTreeState::parse(&sound.to_storage()), Some(sound));
    }

    /// The session-tab entry round-trips the settings grammar whole — both
    /// profile spellings, interleaved with document tabs, scrolls and the
    /// selection intact. Through the file body, not the struct, because the
    /// grammar (commas between entries, a colon before the scroll) is the
    /// thing a new variant could collide with.
    #[test]
    fn a_session_tab_round_trips_the_settings_grammar() {
        use std::str::FromStr as _;
        let node = NodeId::new();
        let built_in = TabRef::Session {
            session: SessionId::mint(),
            profile: SessionProfileKey::BuiltIn(
                BuiltInProfileKey::from_str("powershell").expect("built-in token"),
            ),
            recovery: None,
        };
        let custom = TabRef::Session {
            session: SessionId::mint(),
            profile: SessionProfileKey::Custom(SessionProfileId::mint()),
            recovery: None,
        };
        let before = Settings {
            tabs: vec![
                (TabRef::Welcome, 0.0),
                (built_in.clone(), 0.0),
                (TabRef::Node(node), 120.0),
                (custom.clone(), 0.0),
            ],
            tab: 1,
            ..Settings::default()
        };
        let after = Settings::from_text(&before.profile_body());
        assert_eq!(after.tabs, before.tabs);
        assert_eq!(after.tab, 1, "the selected session tab stays selected");
        // And neither spelling smuggles the list separator in.
        assert!(!built_in.to_storage().contains(','));
        assert!(!custom.to_storage().contains(','));
    }

    /// **A session tab keeps what it needs to come back** (ADR-474): the folder
    /// (a Windows path, whose colon, backslashes and spaces would otherwise be
    /// read as the grammar's), the project, and whether it was a raw terminal.
    #[test]
    fn a_session_tab_round_trips_its_recovery_record_through_the_file() {
        use std::str::FromStr as _;
        let project = ProjectId::mint();
        let tab = TabRef::Session {
            session: SessionId::mint(),
            profile: SessionProfileKey::BuiltIn(
                BuiltInProfileKey::from_str("claude").expect("built-in token"),
            ),
            recovery: Some(SessionRecovery {
                working_directory: Some(r"C:\Users\A B\work, 2026|new:dir".into()),
                project: Some(project),
                raw: true,
            }),
        };
        let line = tab.to_storage();
        for separator in [',', ' '] {
            assert!(
                !line.contains(separator),
                "the entry must not contain {separator:?}: {line}"
            );
        }
        // Scroll still peels from the last colon, and the folder survives it.
        let parsed = parse_tabs(&write_tabs(&[(tab.clone(), 40.0)]));
        assert_eq!(parsed, vec![(tab, 40.0)]);
    }

    /// A field a later build adds, or one that is damaged, costs only itself:
    /// the tab and its other fields come back.
    #[test]
    fn a_recovery_record_reads_past_what_it_does_not_understand() {
        let line = format!(
            "session:{}@claude|raw=1|colour=teal|dir=%ZZ|project=not-a-uuid",
            SessionId::mint()
        );
        let parsed = parse_tabs(&format!("{line}:0"));
        let [(TabRef::Session { recovery, .. }, _)] = parsed.as_slice() else {
            panic!("the tab survives its odd fields: {parsed:?}");
        };
        let recovery = recovery.as_ref().expect("raw=1 was readable");
        assert!(recovery.raw);
        assert_eq!(
            recovery.working_directory, None,
            "a bad escape drops the folder"
        );
        assert_eq!(recovery.project, None, "a bad id drops the project");
    }

    /// An entry with no suffix — every session entry written before this —
    /// reads exactly as it did, with nothing to recover.
    #[test]
    fn a_session_entry_without_a_recovery_suffix_has_no_recovery() {
        let old = format!("session:{}@powershell:0", SessionId::mint());
        let parsed = parse_tabs(&old);
        let [(TabRef::Session { recovery, .. }, _)] = parsed.as_slice() else {
            panic!("an old entry still parses: {parsed:?}");
        };
        assert!(recovery.is_none());
    }

    /// **What a build that has never heard of recovery makes of the new
    /// spelling**: the suffix is part of the profile token it tries to read,
    /// so the entry drops itself and every neighbouring tab survives, which
    /// is the tolerance an unknown session entry already had.
    #[test]
    fn an_older_reader_drops_a_recovering_entry_and_keeps_its_neighbours() {
        use std::str::FromStr as _;
        // The pre-recovery `parse_session`: the profile is everything after the
        // first `@`, and a token with a suffix is not a built-in key.
        fn old_parse_session(rest: &str) -> bool {
            let Some((session, profile)) = rest.split_once('@') else {
                return false;
            };
            SessionId::from_str(session).is_ok()
                && match profile.strip_prefix("custom:") {
                    Some(id) => SessionProfileId::from_str(id).is_ok(),
                    None => BuiltInProfileKey::from_str(profile).is_ok(),
                }
        }
        let with = format!("{}@claude|raw=1", SessionId::mint());
        let without = format!("{}@claude", SessionId::mint());
        assert!(
            !old_parse_session(&with),
            "the suffix makes an older reader drop it"
        );
        assert!(
            old_parse_session(&without),
            "and the plain spelling still reads"
        );
    }

    /// **A terminal look is one line, per agent** (ADR-475): written only for an
    /// agent that chose something, an empty half meaning *follow Settings*.
    #[test]
    fn a_terminal_look_per_agent_round_trips_through_the_profile_file() {
        let mut settings = Settings::default();
        settings.terminal_looks.insert(
            "claude".into(),
            TerminalLook {
                theme: Some("dark".into()),
                font: Some("jetbrains-mono".into()),
            },
        );
        settings.terminal_looks.insert(
            "codex".into(),
            TerminalLook {
                theme: None,
                font: Some("fira-code".into()),
            },
        );
        // Nothing chosen: not written, so the file does not grow with every
        // agent a person ever opened.
        settings
            .terminal_looks
            .insert("bash".into(), TerminalLook::default());
        let body = settings.profile_body();
        assert!(
            body.contains("terminal_looks=claude|dark|jetbrains-mono;codex||fira-code"),
            "{body}"
        );
        let back = Settings::from_text(&body);
        assert_eq!(back.terminal_looks.len(), 2);
        assert_eq!(back.terminal_looks["claude"].theme.as_deref(), Some("dark"));
        assert_eq!(back.terminal_looks["codex"].theme, None);
        assert_eq!(
            back.terminal_looks["codex"].font.as_deref(),
            Some("fira-code")
        );
    }

    /// A file with no such line has no looks, and a damaged line costs only
    /// the entries it damages.
    #[test]
    fn a_missing_or_damaged_terminal_looks_line_costs_only_itself() {
        assert!(
            Settings::from_text("hide_completed=1\n")
                .terminal_looks
                .is_empty()
        );
        let back = Settings::from_text("terminal_looks=;;claude|dark|;||x;|dark|f;codex|light\n");
        let agents: Vec<_> = back.terminal_looks.keys().cloned().collect();
        assert_eq!(agents, ["claude", "codex"], "{:?}", back.terminal_looks);
        assert_eq!(back.terminal_looks["claude"].font, None);
        // Characters that are the line's own grammar cannot be written into a
        // token, so a hand-edited value cannot split an entry.
        let mut settings = Settings::default();
        settings.terminal_looks.insert(
            "bash".into(),
            TerminalLook {
                theme: Some("da;rk|x".into()),
                font: None,
            },
        );
        let reread = Settings::from_text(&settings.profile_body());
        assert_eq!(
            reread.terminal_looks["bash"].theme.as_deref(),
            Some("darkx")
        );
    }

    /// The custom-profile spelling nests a second colon
    /// (`session:<uuid>@custom:<uuid>`), and the scroll suffix is peeled from
    /// the *last* colon — so the two must come apart cleanly, scroll on one
    /// side and the whole profile on the other.
    #[test]
    fn a_custom_profile_session_entry_keeps_scroll_and_profile_apart() {
        let entry = TabRef::Session {
            session: SessionId::mint(),
            profile: SessionProfileKey::Custom(SessionProfileId::mint()),
            recovery: None,
        };
        let parsed = parse_tabs(&write_tabs(&[(entry.clone(), 40.0)]));
        assert_eq!(parsed, vec![(entry, 40.0)]);
    }

    /// **What a session entry means to a build that has never heard of one.**
    /// The pre-Session grammar — the keywords, `doc:`, then a bare node id —
    /// is replicated verbatim below; a `session:` entry must fall through the
    /// whole of it, so an older reader drops that one entry on its own and
    /// keeps every neighbouring document tab. This is the same tolerance the
    /// retired `workspace` token already leans on, pinned for the new variant.
    #[test]
    fn an_older_reader_skips_a_session_entry_and_keeps_the_document_tabs() {
        /// `TabRef::parse` as every build before the Session variant spelled
        /// it, reduced to *what kind of tab came back*.
        fn old_parse(s: &str) -> Option<&'static str> {
            match s {
                "welcome" | "workspaces" | "trash" | "whatsnew" | "draft" => Some("keyword"),
                other => match other.strip_prefix("doc:") {
                    Some(rest) => {
                        let id = match rest.split_once('@') {
                            Some((id, _ws)) => id,
                            None => rest,
                        };
                        koru_core::DocumentId::parse(id).map(|_| "document")
                    }
                    None => NodeId::parse(other).map(|_| "node"),
                },
            }
        }

        let session = TabRef::Session {
            session: SessionId::mint(),
            profile: SessionProfileKey::Custom(SessionProfileId::mint()),
            recovery: None,
        };
        let line = write_tabs(&[
            (TabRef::Welcome, 0.0),
            (session, 0.0),
            (
                TabRef::document(koru_core::DocumentId::new(), Some(WorkspaceId::new())),
                40.0,
            ),
            (TabRef::Node(NodeId::new()), 8.0),
        ]);
        // The old entry loop, verbatim: split on commas, peel a numeric scroll
        // from the last colon, hand the rest to the old parser.
        let survivors: Vec<&str> = line
            .split(',')
            .filter(|e| !e.trim().is_empty())
            .filter_map(|entry| {
                let entry = entry.trim();
                let which = match entry.rsplit_once(':') {
                    Some((w, sc)) if sc.parse::<f32>().is_ok() => w,
                    _ => entry,
                };
                old_parse(which)
            })
            .collect();
        assert_eq!(
            survivors,
            ["keyword", "document", "node"],
            "the unknown session entry dropped itself and nothing else"
        );
    }

    /// Per-document view prefs survive a write and a read, keyed by workspace
    /// and node. A document that follows the defaults is not written at all.
    #[test]
    fn doc_view_prefs_round_trip() {
        let ws = WorkspaceId::new();
        let a = NodeId::new();
        let b = NodeId::new();
        let before = Settings {
            workspace: Some(ws),
            doc_view_prefs: vec![(
                ws,
                vec![
                    (
                        a,
                        DocViewPrefs {
                            mode: Some(ViewMode::Page),
                            canvas_bg: None,
                        },
                    ),
                    (
                        b,
                        DocViewPrefs {
                            mode: Some(ViewMode::Narrow),
                            canvas_bg: Some([0xF5, 0xEF, 0xDF, 0xFF]),
                        },
                    ),
                ],
            )],
            ..Settings::default()
        };
        let after = Settings::from_text(&before.profile_body());
        assert_eq!(after.doc_view_prefs, before.doc_view_prefs);
    }

    /// A document resolves to its own override where it has one and to the
    /// global default everywhere else — per field, and only inside the current
    /// workspace, which is the key the overrides are filed under.
    #[test]
    fn doc_layout_overrides_per_field_within_the_current_workspace() {
        let ws = WorkspaceId::new();
        let other = WorkspaceId::new();
        let mode_only = NodeId::new();
        let elsewhere = NodeId::new();
        let settings = Settings {
            workspace: Some(ws),
            view_mode: ViewMode::Narrow,
            canvas_bg: Some([1, 2, 3, 255]),
            doc_view_prefs: vec![
                (
                    ws,
                    vec![(
                        mode_only,
                        DocViewPrefs {
                            mode: Some(ViewMode::Page),
                            canvas_bg: None,
                        },
                    )],
                ),
                (
                    other,
                    vec![(
                        elsewhere,
                        DocViewPrefs {
                            mode: Some(ViewMode::Full),
                            canvas_bg: None,
                        },
                    )],
                ),
            ],
            ..Settings::default()
        };
        assert_eq!(
            settings.doc_layout(Some(mode_only)),
            (ViewMode::Page, Some([1, 2, 3, 255])),
            "the mode is overridden, the colour still follows the default"
        );
        assert_eq!(
            settings.doc_layout(Some(NodeId::new())),
            (ViewMode::Narrow, Some([1, 2, 3, 255])),
            "a document with no entry follows both defaults"
        );
        assert_eq!(
            settings.doc_layout(Some(elsewhere)),
            (ViewMode::Narrow, Some([1, 2, 3, 255])),
            "another workspace's override is not this workspace's"
        );
        assert_eq!(
            settings.doc_layout(None),
            (ViewMode::Narrow, Some([1, 2, 3, 255])),
            "no root means the defaults"
        );
    }

    /// An override with every field unset is dropped on write — the file records
    /// only what is genuinely personal to the document.
    #[test]
    fn an_empty_doc_pref_entry_is_not_written() {
        let ws = WorkspaceId::new();
        let a = NodeId::new();
        let before = Settings {
            workspace: Some(ws),
            doc_view_prefs: vec![(ws, vec![(a, DocViewPrefs::default())])],
            ..Settings::default()
        };
        let after = Settings::from_text(&before.profile_body());
        assert!(
            after.doc_view_prefs.is_empty(),
            "an all-default entry should not survive"
        );
    }

    /// The profile file must not carry a *stale* copy of anything that moved
    /// out: a hand-edited preference key in a profile folder that silently does
    /// nothing is worse than no key at all. Every preference is the settings
    /// authority's now, so none of them may appear here — and neither may the
    /// geometry that went to `windows.conf`.
    #[test]
    fn the_profile_file_no_longer_writes_what_moved_out() {
        let s = Settings {
            pos: Some((10, 20)),
            size: (1200.0, 800.0),
            maximized: true,
            font: FontSetting::Chosen(koru_settings::FontFamily::IbmPlexSans),
            zoom: 1.25,
            pinned: true,
            popout_close: PopoutClose::RestoreTab,
            ..Settings::default()
        };
        let body = s.profile_body();
        for moved in [
            "x=",
            "y=",
            "w=",
            "h=",
            "maximized=",
            "pinned=",
            "dark=",
            "theme=",
            "font=",
            "zoom=",
            "density=",
            "caret_color=",
            "popout_close=",
            "view_mode=",
            "canvas_bg=",
            "emoji_skin_tone=",
            "dictation_toggle=",
        ] {
            assert!(
                !body.lines().any(|l| l.starts_with(moved)),
                "{moved} belongs to the settings authority or windows.conf now"
            );
        }
        // ...and an old profile file is still understood for its own keys.
        let old = Settings::from_text("x=10\ny=20\nw=800\nh=600\nai_w=500\n");
        assert_eq!(old.pos, Some((10, 20)));
        assert_eq!(old.ai_w, 500.0);
    }

    /// **The two that came the other way** (ADR-266). Neither is a preference,
    /// so both live in the session file this host owns — and a session that
    /// holds neither writes neither, rather than growing two empty keys.
    #[test]
    fn the_relocated_keys_round_trip_through_the_profile_file() {
        let s = Settings {
            emoji_recents: vec!["🙂".into(), "🚀".into()],
            last_export_dir: Some(PathBuf::from("/tmp/exports")),
            ..Settings::default()
        };
        let body = s.profile_body();
        assert!(body.contains("emoji_recents=🙂,🚀\n"), "{body}");
        assert!(body.contains("last_export_dir=/tmp/exports\n"), "{body}");

        let back = Settings::from_text(&body);
        assert_eq!(back.emoji_recents, s.emoji_recents);
        assert_eq!(back.last_export_dir, s.last_export_dir);

        let empty = Settings::default().profile_body();
        assert!(!empty.contains("emoji_recents="), "{empty}");
        assert!(!empty.contains("last_export_dir="), "{empty}");
    }

    /// **The AI chats' seen marks** (koru-ai D83) ride the session file as
    /// one opaque line, beside the tabs, and none is written while there are
    /// none. A file carrying it still opens every other key as before — the
    /// tolerance an older build's `from_text` gives a key it does not know.
    #[test]
    fn the_chat_seen_line_round_trips_and_is_absent_when_empty() {
        let line = "8b0c4d1e-0000-4000-8000-000000000001:12:-:u";
        let s = Settings {
            chat_seen: line.into(),
            ai_w: 420.0,
            ..Settings::default()
        };
        let body = s.profile_body();
        assert!(body.contains(&format!("chat_seen={line}\n")), "{body}");
        let back = Settings::from_text(&body);
        assert_eq!(back.chat_seen, line);
        assert_eq!(back.ai_w, 420.0);
        assert!(!Settings::default().profile_body().contains("chat_seen="));
    }

    /// **The rail's order** rides the session file the same way: one opaque
    /// line beside the tabs, none while the rail is in its default order.
    ///
    /// And what an older build makes of it, shown with a key *this* build does
    /// not know: `from_text` passes over it, every other key still reads, and
    /// the next save drops it. That is all a build from before `rail_order=`
    /// does with the line — its rail draws the default order.
    #[test]
    fn the_rail_order_line_round_trips_and_a_build_that_does_not_know_it_ignores_it() {
        let line = "chats,documents,files";
        let s = Settings {
            rail_order: line.into(),
            ai_w: 420.0,
            ..Settings::default()
        };
        let body = s.profile_body();
        assert!(body.contains(&format!("rail_order={line}\n")), "{body}");
        let back = Settings::from_text(&body);
        assert_eq!(back.rail_order, line);
        assert_eq!(back.ai_w, 420.0);
        assert!(!Settings::default().profile_body().contains("rail_order="));

        let newer = body.replace("rail_order=", "rail_order_from_a_newer_build=");
        let older = Settings::from_text(&newer);
        assert_eq!(older.ai_w, 420.0, "every other key still reads");
        assert_eq!(older.rail_order, "", "the unknown line is passed over");
        assert!(
            !older
                .profile_body()
                .contains("rail_order_from_a_newer_build="),
            "and dropped by the next save"
        );
    }

    /// A ranking longer than the picker keeps is trimmed on the way in, so a
    /// hand-edited file cannot make the session file grow without bound.
    #[test]
    fn the_emoji_ranking_is_capped_on_read() {
        let many: Vec<String> = (0..80).map(|i| format!("e{i}")).collect();
        let back = Settings::from_text(&format!("emoji_recents={}\n", many.join(",")));
        assert_eq!(back.emoji_recents.len(), MAX_EMOJI_RECENTS);
        assert_eq!(back.emoji_recents[0], "e0");
    }

    /// **The bug this keying exists for.** With one flat list, switching workspaces
    /// wrote the new workspace's set over the old one's *and* restored ids that
    /// named nothing in the new document — so every tree came back expanded and the
    /// state you left behind was gone.
    #[test]
    fn each_workspace_keeps_its_own_collapsed_set() {
        let one = WorkspaceId::new();
        let two = WorkspaceId::new();
        let (a, b, c) = (NodeId::new(), NodeId::new(), NodeId::new());

        let mut s = Settings::default();
        s.set_collapsed_in(one, vec![a, b]);
        s.set_collapsed_in(two, vec![c]);

        assert_eq!(s.collapsed_in(one), [a, b]);
        assert_eq!(s.collapsed_in(two), [c]);

        // Recording one again leaves the other alone — writing the field is what
        // did not.
        s.set_collapsed_in(two, vec![]);
        assert_eq!(s.collapsed_in(one), [a, b], "the other workspace survived");
        assert!(
            s.collapsed_in(WorkspaceId::new()).is_empty(),
            "and an unknown one is empty"
        );
    }

    /// It reaches disk per workspace, and comes back the same way.
    #[test]
    fn the_collapsed_sets_round_trip_one_line_per_workspace() {
        let one = WorkspaceId::new();
        let two = WorkspaceId::new();
        let (a, b, c) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mut before = Settings::default();
        before.set_collapsed_in(one, vec![a, b]);
        before.set_collapsed_in(two, vec![c]);

        let body = before.profile_body();
        assert_eq!(
            body.lines().filter(|l| l.starts_with("collapsed.")).count(),
            2,
            "one line per workspace"
        );

        let after = Settings::from_text(&body);
        assert_eq!(after.collapsed_in(one), [a, b]);
        assert_eq!(after.collapsed_in(two), [c]);
    }

    /// A workspace with nothing shut in it earns no line, so the file does not grow
    /// a row for every workspace you have ever glanced at.
    #[test]
    fn a_workspace_with_nothing_collapsed_is_not_written() {
        let mut s = Settings::default();
        s.set_collapsed_in(WorkspaceId::new(), vec![]);
        assert!(!s.profile_body().contains("collapsed."));
    }

    /// ...and a workspace that has been removed stops being remembered at all —
    /// the same leak `ExploreView::collapsed` prevents one level down.
    #[test]
    fn a_workspace_that_is_gone_is_pruned() {
        let live = WorkspaceId::new();
        let gone = WorkspaceId::new();
        let mut s = Settings::default();
        s.set_collapsed_in(live, vec![NodeId::new()]);
        s.set_collapsed_in(gone, vec![NodeId::new()]);

        s.prune_collapsed(&[live]);
        assert_eq!(s.collapsed.len(), 1);
        assert!(s.collapsed_in(gone).is_empty());
    }

    /// A hand-edited line naming something that is not a workspace is dropped
    /// rather than folded into somebody else's set — the ids in it mean nothing
    /// outside the workspace they came from.
    #[test]
    fn a_collapsed_line_with_an_unreadable_workspace_is_dropped() {
        let s = Settings::from_text(&format!(
            "collapsed.not-a-uuid={}\n",
            NodeId::new().to_storage()
        ));
        assert!(s.collapsed.is_empty());
    }

    /// A settings file written by a build that had never heard of tabs must
    /// still open — every new key simply takes its default.
    #[test]
    fn a_file_from_an_older_build_still_loads() {
        let s = Settings::from_text("x=1\ny=2\nw=940\nh=620\ndark=1\npinned=0\n");
        assert_eq!(s.pos, Some((1, 2)));
        assert!(s.tabs.is_empty(), "no tabs recorded");
        assert!(s.sidebar_open, "the explore column defaults open");
        assert!(!s.context_open, "the context column defaults shut");
        assert!(!s.maximized);
        assert_eq!(s.tab, 0);
    }

    /// One malformed entry drops itself. Losing a tab is survivable; losing the
    /// session because of one bad line is not.
    #[test]
    fn a_malformed_tab_drops_without_taking_its_neighbours() {
        let id = NodeId::new();
        let text = format!("tabs=welcome,not-a-uuid,{}:40,\n", id.to_storage());
        let s = Settings::from_text(&text);
        assert_eq!(
            s.tabs,
            vec![(TabRef::Welcome, 0.0), (TabRef::Node(id), 40.0)]
        );
    }

    /// A session written before the outline stopped being a tab. `workspace` is
    /// no longer a word this parser knows, so that entry drops on its own and
    /// every other tab comes back — no migration, and nothing to remember.
    #[test]
    fn a_session_from_before_the_workspace_tab_keeps_its_other_tabs() {
        let id = NodeId::new();
        let text = format!("tabs=workspace:120,welcome,{}:40\ntab=2\n", id.to_storage());
        let s = Settings::from_text(&text);
        assert_eq!(
            s.tabs,
            vec![(TabRef::Welcome, 0.0), (TabRef::Node(id), 40.0)]
        );
        assert_eq!(s.tab, 1, "and the index is clamped to what is left");
    }

    /// **A document tab survives the file.**
    ///
    /// The entry is written `doc:<uuid>:<scroll>` and read back by splitting off
    /// the scroll — so splitting at the *first* colon read the reference as
    /// `doc`, which parses as nothing, and every document tab vanished on the
    /// way in. Nothing failed; the strip was simply shorter next launch.
    #[test]
    fn a_document_tab_round_trips_through_the_file() {
        let doc = koru_core::DocumentId::new();
        let ws = WorkspaceId::new();
        let before = Settings {
            workspace: Some(ws),
            tabs: vec![
                (TabRef::Welcome, 0.0),
                (TabRef::document(doc, Some(ws)), 40.0),
            ],
            tab: 1,
            ..Settings::default()
        };
        let after = Settings::from_text(&before.profile_body());
        assert_eq!(after.tabs, before.tabs);
        assert_eq!(after.tab, 1);
    }

    /// **A node id and a document id are both v4 uuids**, so the prefix is the
    /// only thing keeping them apart. A `doc:` that names nothing must not come
    /// back as a node — it would name a node that does not exist and the tab
    /// would drop itself for the wrong reason.
    #[test]
    fn a_document_reference_is_never_read_as_a_node() {
        let id = koru_core::DocumentId::new();
        let s = Settings::from_text(&format!("tabs=doc:{}:0\n", id.to_storage()));
        assert_eq!(s.tabs, vec![(TabRef::document(id, None), 0.0)]);

        // ...and the other way round: a bare uuid is still a node.
        let node = NodeId::new();
        let s = Settings::from_text(&format!("tabs={}:0\n", node.to_storage()));
        assert_eq!(s.tabs, vec![(TabRef::Node(node), 0.0)]);
    }

    /// A bare `doc:` line picks up the window's workspace as home on load.
    #[test]
    fn a_legacy_document_tab_inherits_the_window_workspace() {
        let ws = WorkspaceId::new();
        let id = koru_core::DocumentId::new();
        let s = Settings::from_text(&format!(
            "workspace={}\ntabs=doc:{}:0\n",
            ws.to_storage(),
            id.to_storage()
        ));
        assert_eq!(s.tabs, vec![(TabRef::document(id, Some(ws)), 0.0)]);
    }

    /// The Main strip is window-scoped: adopting another workspace does not
    /// clear or replace the open tabs.
    #[test]
    fn adopting_a_workspace_leaves_the_open_strip_alone() {
        let (one, two) = (WorkspaceId::new(), WorkspaceId::new());
        let a = koru_core::DocumentId::new();
        let mut s = Settings {
            workspace: Some(one),
            tabs: vec![
                (TabRef::Welcome, 0.0),
                (TabRef::document(a, Some(one)), 0.0),
            ],
            tab: 1,
            ..Settings::default()
        };
        s.adopt_tabs_of(two);
        assert_eq!(
            s.tabs,
            vec![
                (TabRef::Welcome, 0.0),
                (TabRef::document(a, Some(one)), 0.0)
            ]
        );
        assert_eq!(s.tab, 1);
        assert_eq!(s.open_document_ids(), vec![a]);
    }

    /// A file written before tabs were kept per workspace still restores its
    /// strip from `tabs=` — the window-scoped list is what matters now.
    #[test]
    fn a_session_from_before_per_workspace_tabs_keeps_its_strip() {
        let ws = WorkspaceId::new();
        let node = NodeId::new();
        let text = format!(
            "workspace={}\ntabs=welcome,{}:40\ntab=1\n",
            ws.to_storage(),
            node.to_storage()
        );
        let s = Settings::from_text(&text);
        assert_eq!(
            s.tabs,
            vec![(TabRef::Welcome, 0.0), (TabRef::Node(node), 40.0)],
            "the one strip such a file remembers must not be thrown away"
        );
        assert_eq!(s.tab, 1);
    }

    /// An index written by a run that had more tabs open must not select past
    /// the end of this one.
    #[test]
    fn an_out_of_range_active_tab_is_clamped() {
        let s = Settings::from_text("tabs=welcome,welcome\ntab=9\n");
        assert_eq!(s.tab, 1, "clamped to the last real tab");

        let s = Settings::from_text("tabs=\ntab=4\n");
        assert!(s.tabs.is_empty());
        assert_eq!(s.tab, 0, "no tabs means no index");
    }

    /// A scroll offset that is NaN or infinite would poison every later clamp,
    /// and `f32::parse` accepts both spellings.
    #[test]
    fn a_nonsense_scroll_offset_becomes_the_top() {
        let s = Settings::from_text("tabs=welcome:NaN,welcome:inf,welcome:-20\n");
        assert_eq!(s.tabs.len(), 3);
        for (_, scroll) in &s.tabs {
            assert!(scroll.is_finite() && *scroll >= 0.0, "got {scroll}");
        }
    }

    /// The list is capped, so a long-running install cannot grow the file
    /// without bound.
    #[test]
    fn recents_are_capped() {
        let ids: Vec<String> = (0..20).map(|_| DocumentId::new().to_storage()).collect();
        let s = Settings::from_text(&format!("recents={}\n", ids.join(",")));
        assert_eq!(s.recents.len(), MAX_RECENTS);
    }

    /// **The update keys are read, ignored and dropped — deliberately.**
    ///

    /// The session belongs to the content it is a session over. Two data
    /// directories must never share one — that was the whole reason for moving
    /// the file out of the per-user config directory.
    #[test]
    fn the_settings_file_lives_inside_the_profile_directory() {
        let dev = Path::new("/data/dev");
        let real = Path::new("/data/real");
        assert_eq!(
            config_path(dev, "A"),
            Path::new("/data/dev/settings-A.conf")
        );
        assert_ne!(config_path(dev, "A"), config_path(real, "A"));
        // Two peers must never share a file, or they fight over the geometry.
        assert_ne!(config_path(dev, "A"), config_path(dev, "B"));
    }

    #[test]
    fn startup_reconciliation_drops_only_removed_profile_window_rows() {
        let live = koru_core::ProfileId::new();
        let removed = koru_core::ProfileId::new();
        let body = format!(
            "{} | default=live\n{} | default=removed\nlegacy line\n",
            live.to_storage(),
            removed.to_storage(),
        );
        let live_profiles = [live].into_iter().collect();
        assert_eq!(
            retain_live_profile_window_rows(&body, &live_profiles),
            format!("{} | default=live\nlegacy line\n", live.to_storage())
        );
    }

    #[test]
    fn startup_reconciliation_prunes_removed_workspace_session_state() {
        let kept = WorkspaceId::new();
        let removed = WorkspaceId::new();
        let mut settings = Settings::default();
        settings.workspace = Some(removed);
        settings.sessions = vec![(removed, Vec::new(), 0), (kept, Vec::new(), 0)];
        settings.retain_workspace_state(&[kept].into_iter().collect());
        assert_eq!(settings.workspace, None);
        assert_eq!(settings.sessions, vec![(kept, Vec::new(), 0)]);
    }

    #[test]
    fn profile_mru_round_trips_and_reorders() {
        let first = koru_core::ProfileId::new();
        let second = koru_core::ProfileId::new();
        let mut settings = Settings::default();
        settings.remember_profile(first);
        settings.remember_profile(second);
        settings.remember_profile(first);
        assert_eq!(settings.profile_mru, vec![first, second]);
        let loaded = Settings::from_text(&settings.profile_body());
        assert_eq!(loaded.profile_mru, vec![first, second]);
    }
}

/// `key = value` lines, tolerant of blanks, comments and a byte-order mark —
/// the same stance every other conf here takes.
fn key_values(text: &str) -> Vec<(&str, &str)> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}
