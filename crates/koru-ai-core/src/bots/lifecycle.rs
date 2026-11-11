// SPDX-License-Identifier: MPL-2.0

//! The closed vocabularies of recoverable Bot lifecycle (P1.7, ai.db 18).
//!
//! Each enum here is the only writer and reader of its stored column: a
//! storage token is produced by [`token`](BotLifecycleKind::token) and read
//! back by `parse`, and `koru-ai-sqlite` tests that every list matches its
//! SQL `CHECK` in both directions, so a value cannot be added on one side
//! only. Unknown stored tokens fail decoding instead of becoming a default.

use super::{BotId, BotRequestId};
use crate::{ChatAttachmentId, ChatId, DocumentId, WorkspaceId};
use koru_delegate_core::RunId;
use serde::{Deserialize, Serialize};

macro_rules! closed_vocabulary {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            /// The stored token.
            pub const fn token(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }
            /// The value a stored token names, or `None` for anything else.
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($token => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

closed_vocabulary! {
    /// A Bot's stored lifecycle (`bots.lifecycle`, ai16). Pause is separate.
    BotLifecycle { Active => "active", Archived => "archived", Trashed => "trashed" }
}

impl Default for BotLifecycle {
    fn default() -> Self {
        Self::Active
    }
}
impl BotLifecycle {
    pub fn is_active(&self) -> bool {
        *self == Self::Active
    }
}

/// A Bot's unfinished lifecycle operation as its read reports it: enough to
/// resume it (the same command kind with this `request_id`) or to reverse it
/// (a Restore naming it), with why it stopped when it is parked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotOpenLifecycle {
    pub request_id: BotRequestId,
    pub kind: BotLifecycleKind,
    pub chat_id: Option<ChatId>,
    pub stage: BotLifecycleStage,
    pub refusal: Option<BotLifecycleRefusal>,
}

closed_vocabulary! {
    /// Why a conversation belongs to its Bot (`bot_chats.purpose`, ai16).
    BotChatPurpose { Main => "main", Additional => "additional" }
}

closed_vocabulary! {
    /// What a lifecycle operation does.
    BotLifecycleKind {
        Archive => "archive",
        Trash => "trash",
        Restore => "restore",
        TrashChat => "trash_chat",
        RestoreChat => "restore_chat",
        PurgeBot => "purge_bot",
        PurgeChat => "purge_chat",
    }
}

closed_vocabulary! {
    /// Where an operation has got to. `Incomplete` is parked and retryable;
    /// only `Completed` is final.
    BotLifecycleStage {
        Captured => "captured",
        Settling => "settling",
        Applying => "applying",
        Restoring => "restoring",
        PreservingReferences => "preserving_references",
        Purging => "purging",
        Completed => "completed",
        Incomplete => "incomplete",
    }
}

closed_vocabulary! {
    /// Why an operation is parked as incomplete.
    BotLifecycleRefusal {
        OwnerUnavailable => "owner_unavailable",
        WorkRunning => "work_running",
        // Reserved and unused: Archive and Trash end a held session as the
        // person's Stop (O27 reversed), so nothing parks for it. It stays
        // because ai18's CHECK list, which is committed, names it.
        PersonHolding => "person_holding",
        MemberConflict => "member_conflict",
        StoreUnavailable => "store_unavailable",
        RevisionChanged => "revision_changed",
    }
}

closed_vocabulary! {
    /// The state of the effects-off policy barrier a whole-Bot operation installs.
    BotLifecycleBarrierState { Pending => "pending", Effective => "effective" }
}

closed_vocabulary! {
    /// What a captured member is.
    BotLifecycleMemberKind {
        Bot => "bot",
        Chat => "chat",
        RunReference => "run_reference",
        OwnedRole => "owned_role",
        AttachmentCopy => "attachment_copy",
    }
}

closed_vocabulary! {
    /// The authoritative owner that applies a member's effect.
    BotLifecycleOwner { Ai => "ai", Delegate => "delegate", Store => "store" }
}

closed_vocabulary! {
    /// The proof that put a member in the operation.
    BotLifecycleReason {
        Bot => "bot",
        OwnedConversation => "owned_conversation",
        RunOfConversation => "run_of_conversation",
        RoleCreationReceipt => "role_creation_receipt",
        ExclusiveAttachment => "exclusive_attachment",
    }
}

closed_vocabulary! {
    /// A member's state when it was captured.
    BotLifecyclePriorState {
        Live => "live",
        Trashed => "trashed",
        Archived => "archived",
        Absent => "absent",
    }
}

closed_vocabulary! {
    /// What the operation means to do to a member.
    BotLifecycleIntended { Hide => "hide", Restore => "restore", Retain => "retain", Remove => "remove" }
}

closed_vocabulary! {
    /// A member's progress with its owner.
    BotLifecycleMemberStage {
        Pending => "pending",
        Acknowledged => "acknowledged",
        Retained => "retained",
        Unavailable => "unavailable",
        Conflict => "conflict",
    }
}

closed_vocabulary! {
    /// Why a member was kept rather than changed. Shown in the result.
    BotLifecycleRetained {
        SharedReference => "shared_reference",
        AlreadyTrashed => "already_trashed",
        UncapturedDescendant => "uncaptured_descendant",
        NativeResult => "native_result",
        OwnershipUnproven => "ownership_unproven",
        PathAttachment => "path_attachment",
        OwnerUnavailable => "owner_unavailable",
    }
}

closed_vocabulary! {
    /// What a purge tombstone stands for.
    BotTombstoneSubject { Bot => "bot", Chat => "chat" }
}

/// What a captured member is, with exactly the identities its kind stores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "subject", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotLifecycleSubject {
    Bot,
    Chat {
        chat_id: ChatId,
    },
    /// A Run produced in `chat_id`. Its document is the delegate's to name
    /// and may be recorded after capture.
    RunReference {
        run_id: RunId,
        workspace_id: WorkspaceId,
        document_id: Option<DocumentId>,
        chat_id: Option<ChatId>,
    },
    OwnedRole {
        workspace_id: WorkspaceId,
        document_id: DocumentId,
    },
    AttachmentCopy {
        chat_id: ChatId,
        attachment_id: ChatAttachmentId,
    },
}

impl BotLifecycleSubject {
    pub fn kind(&self) -> BotLifecycleMemberKind {
        match self {
            Self::Bot => BotLifecycleMemberKind::Bot,
            Self::Chat { .. } => BotLifecycleMemberKind::Chat,
            Self::RunReference { .. } => BotLifecycleMemberKind::RunReference,
            Self::OwnedRole { .. } => BotLifecycleMemberKind::OwnedRole,
            Self::AttachmentCopy { .. } => BotLifecycleMemberKind::AttachmentCopy,
        }
    }
    /// The owner that applies this member's effect. Fixed by kind, so a
    /// member can never name an owner that cannot act on it.
    pub fn owner(&self) -> BotLifecycleOwner {
        match self {
            Self::Bot | Self::Chat { .. } | Self::AttachmentCopy { .. } => BotLifecycleOwner::Ai,
            Self::RunReference { .. } => BotLifecycleOwner::Delegate,
            Self::OwnedRole { .. } => BotLifecycleOwner::Store,
        }
    }
}

/// One member's outcome, as a lifecycle result reports it. Internal owner
/// correlation (child request IDs) is not part of the public result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotLifecycleMemberOutcome {
    pub subject: BotLifecycleSubject,
    pub intended: BotLifecycleIntended,
    pub stage: BotLifecycleMemberStage,
    /// Why the member was kept, when it was.
    pub retained: Option<BotLifecycleRetained>,
}

/// Where a lifecycle operation stands. A pending or incomplete operation is
/// never reported as done; owner timeouts stay visible as `incomplete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotLifecycleProgress {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub kind: BotLifecycleKind,
    pub chat_id: Option<ChatId>,
    pub source_request_id: Option<BotRequestId>,
    pub stage: BotLifecycleStage,
    pub refusal: Option<BotLifecycleRefusal>,
    pub members: Vec<BotLifecycleMemberOutcome>,
}

impl BotLifecycleKind {
    /// A conversation operation names its Chat; a whole-Bot one does not.
    pub const fn names_chat(self) -> bool {
        matches!(self, Self::TrashChat | Self::RestoreChat | Self::PurgeChat)
    }
    /// Restore and purge act on the members an earlier operation captured.
    pub const fn needs_source(self) -> bool {
        matches!(
            self,
            Self::Restore | Self::RestoreChat | Self::PurgeBot | Self::PurgeChat
        )
    }
    /// Archive and Trash of an active Bot pause it before any owner effect.
    pub const fn pauses(self, prior: BotLifecycle) -> bool {
        matches!(self, Self::Archive | Self::Trash) && matches!(prior, BotLifecycle::Active)
    }
}

#[cfg(test)]
mod tests;
