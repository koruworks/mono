// SPDX-License-Identifier: MPL-2.0

//! The six `bots.*` rows (M8): team pause, and the streams and copies of
//! this person's linked computers.
//!
//! **Every value here is an id, a number, a flag or a variant of a closed
//! enum.** There is no field a Bot's name, a conversation, a message, a
//! path, a credential or the name of a person or a computer could be put
//! in. An emitter that holds a code as text ([`BotsSessionReason`],
//! [`BotsCommandKind`], [`BotsCommandFailure`]) looks it up with
//! `from_token`; a code outside the set is `Other`, never the text.

use uuid::Uuid;

use super::{Draft, EventPayload, sealed};
use crate::envelope::{AttrValue, EventName};

/// A closed set of tokens as an enum: `token` is what the registry row's
/// vocabulary declares, `from_token` is the lookup for a code held as text.
macro_rules! tokens {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$doc:meta])* $variant:ident => $token:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum $name {
            $($(#[$doc])* $variant),+
        }

        impl $name {
            /// Every variant, in the order of the registry's vocabulary.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn token(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }

            /// The variant a token names, when it names one.
            pub fn from_token(token: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.token() == token)
            }
        }
    };
}

tokens!(
    /// `bots.team_pause.changed`'s `stage`.
    BotsTeamPauseStage {
        Pending => "pending",
        Acknowledged => "acknowledged",
    }
);

tokens!(
    /// `bots.client.session`'s `stage`.
    BotsSessionStage {
        Admitted => "admitted",
        Refused => "refused",
        Closed => "closed",
    }
);

tokens!(
    /// The role of a Bot client stream.
    BotsClientRole {
        Feed => "feed",
        Command => "command",
        Live => "live",
    }
);

tokens!(
    /// Why a Bot client stream was refused or closed.
    BotsSessionReason {
        UnsupportedVersion => "unsupported_version",
        PrincipalMismatch => "principal_mismatch",
        NotOwnDevice => "not_own_device",
        WrongOwner => "wrong_owner",
        Busy => "busy",
        BotsUnavailable => "bots_unavailable",
        UnsupportedMessage => "unsupported_message",
        ProtocolError => "protocol_error",
        /// The other end closed the stream, or the carrier ended it.
        Closed => "closed",
        /// The other end did not answer in the time it had.
        Timeout => "timeout",
        /// Nothing was asked on the stream for as long as it may stay open.
        Idle => "idle",
        Other => "other",
    }
);

tokens!(
    /// Which command a linked device sent.
    BotsCommandKind {
        SendTurn => "send_turn",
        RetryTurn => "retry_turn",
        CancelAttempt => "cancel_attempt",
        Steer => "steer",
        CreateChat => "create_chat",
        PauseBot => "pause_bot",
        ResumeBot => "resume_bot",
        PauseTeam => "pause_team",
        ResumeTeam => "resume_team",
        AnswerDecision => "answer_decision",
        UpdateBot => "update_bot",
        Other => "other",
    }
);

tokens!(
    /// What a receipt records.
    BotsCommandOutcome {
        Completed => "completed",
        Accepted => "accepted",
        Refused => "refused",
        Unknown => "unknown",
    }
);

tokens!(
    /// The code a receipt keeps.
    BotsCommandFailure {
        ExecutingComputerOnly => "executing_computer_only",
        NotApplied => "not_applied",
        Busy => "busy",
        ChatBusy => "chat_busy",
        InvalidRequest => "invalid_request",
        NotFound => "not_found",
        WrongOwner => "wrong_owner",
        WorkspaceUnavailable => "workspace_unavailable",
        RequestConflict => "request_conflict",
        RevisionConflict => "revision_conflict",
        FolderLocked => "folder_locked",
        StorageError => "storage_error",
        BotOperationPending => "bot_operation_pending",
        TeamPaused => "team_paused",
        TeamRevisionConflict => "team_revision_conflict",
        BotsUnavailable => "bots_unavailable",
        BotUnavailable => "bot_unavailable",
        BotStale => "bot_stale",
        AiWorkerTimeout => "ai_worker_timeout",
        AiUnavailable => "ai_unavailable",
        Refused => "refused",
        Other => "other",
    }
);

tokens!(
    /// Why the owner did not accept a client's cursor.
    BotsResetReason {
        FirstSubscribe => "first_subscribe",
        FeedReplaced => "feed_replaced",
        FeedRewound => "feed_rewound",
        ClientRequested => "client_requested",
    }
);

tokens!(
    /// What a copy is shown as.
    BotsMirrorState {
        Never => "never",
        Current => "current",
        Stale => "stale",
        Unsupported => "unsupported",
        Withdrawing => "withdrawing",
    }
);

tokens!(
    /// What happened to a copy.
    BotsMirrorReason {
        /// The copy's row was made: the computer became a linked one.
        Linked => "linked",
        /// A page brought the copy up to date.
        CaughtUp => "caught_up",
        /// The feed stream ended.
        StreamLost => "stream_lost",
        /// This worker started: no subscription is open yet.
        WorkerStarted => "worker_started",
        /// The two computers share no protocol version or copy format.
        NeedsUpdate => "needs_update",
        /// The computer stopped being a linked one; the copy is being
        /// deleted.
        Unlinked => "unlinked",
        ResetStarted => "reset_started",
        ResetFinished => "reset_finished",
        /// A reset ended before it finished; what it had received is gone.
        ResetDropped => "reset_dropped",
    }
);

tokens!(
    /// Why a copy was deleted.
    BotsMirrorRemovedReason {
        Unlinked => "unlinked",
    }
);

fn named(name: &'static str) -> Draft {
    Draft::new(EventName::from_static(name))
}

fn token(value: &'static str) -> AttrValue {
    AttrValue::Token(value.to_owned())
}

/// `bots.team_pause.changed`.
pub struct BotsTeamPauseChanged {
    pub owner_device: Uuid,
    pub epoch: u64,
    pub paused: bool,
    pub stage: BotsTeamPauseStage,
}

payload!(BotsTeamPauseChanged, "bots.team_pause.changed", |self| {
    named(Self::NAME)
        .attr("owner_device", AttrValue::Id(self.owner_device))
        .attr("epoch", AttrValue::Count(self.epoch))
        .attr("paused", AttrValue::Flag(self.paused))
        .attr("stage", token(self.stage.token()))
});

/// `bots.client.session`.
pub struct BotsClientSession {
    pub peer_device: Uuid,
    pub stage: BotsSessionStage,
    /// `None` when no hello was read.
    pub role: Option<BotsClientRole>,
    /// `None` when no version was agreed.
    pub protocol_version: Option<u32>,
    /// `None` for `admitted`.
    pub reason: Option<BotsSessionReason>,
}

payload!(BotsClientSession, "bots.client.session", |self| {
    let mut draft = named(Self::NAME)
        .attr("peer_device", AttrValue::Id(self.peer_device))
        .attr("stage", token(self.stage.token()));
    if let Some(role) = self.role {
        draft = draft.attr("role", token(role.token()));
    }
    if let Some(version) = self.protocol_version {
        draft = draft.attr("protocol_version", AttrValue::Count(u64::from(version)));
    }
    if let Some(reason) = self.reason {
        draft = draft.attr("reason", token(reason.token()));
    }
    draft
});

/// `bots.client.command`.
pub struct BotsClientCommand {
    pub peer_device: Uuid,
    pub request: Uuid,
    pub kind: BotsCommandKind,
    pub outcome: BotsCommandOutcome,
    pub failure: Option<BotsCommandFailure>,
}

payload!(BotsClientCommand, "bots.client.command", |self| {
    let draft = named(Self::NAME)
        .attr("peer_device", AttrValue::Id(self.peer_device))
        .attr("request", AttrValue::Id(self.request))
        .attr("kind", token(self.kind.token()))
        .attr("outcome", token(self.outcome.token()));
    match self.failure {
        Some(failure) => draft.attr("failure", token(failure.token())),
        None => draft,
    }
});

/// `bots.client.reset`.
pub struct BotsClientReset {
    pub peer_device: Uuid,
    pub reason: BotsResetReason,
}

payload!(BotsClientReset, "bots.client.reset", |self| {
    named(Self::NAME)
        .attr("peer_device", AttrValue::Id(self.peer_device))
        .attr("reason", token(self.reason.token()))
});

/// `bots.mirror.state`.
pub struct BotsMirrorStateChanged {
    pub owner_device: Uuid,
    pub state: BotsMirrorState,
    pub reason: BotsMirrorReason,
    pub cursor: u64,
}

payload!(BotsMirrorStateChanged, "bots.mirror.state", |self| {
    named(Self::NAME)
        .attr("owner_device", AttrValue::Id(self.owner_device))
        .attr("state", token(self.state.token()))
        .attr("reason", token(self.reason.token()))
        .attr("cursor", AttrValue::Count(self.cursor))
});

/// `bots.mirror.removed`.
pub struct BotsMirrorRemoved {
    pub owner_device: Uuid,
    pub reason: BotsMirrorRemovedReason,
}

payload!(BotsMirrorRemoved, "bots.mirror.removed", |self| {
    named(Self::NAME)
        .attr("owner_device", AttrValue::Id(self.owner_device))
        .attr("reason", token(self.reason.token()))
});
