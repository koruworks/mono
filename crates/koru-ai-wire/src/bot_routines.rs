// SPDX-License-Identifier: MPL-2.0
//! Native Routine occurrences retain their own identities, never Run IDs.
use koru_ai_core::{
    ChatId,
    bots::{BotId, BotPage, BotRoutineControl, BotRoutineDetail},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "BotRoutineCommand")]
pub enum Command {
    Create {
        id: Uuid,
        bot: BotId,
        expected_bot_revision: u64,
        name: String,
        objective: String,
        recurrence: koru_ai_core::bots::RoutineRecurrenceRule,
    },
    Reviews {
        bot: BotId,
        before: Option<Cursor>,
    },
    Review {
        bot: BotId,
        occurrence: Uuid,
        offset: usize,
    },
    SkipReview {
        id: Uuid,
        bot: BotId,
        occurrence: Uuid,
    },
    ClaimStartup {
        id: Uuid,
        bot: BotId,
    },
    History {
        bot: BotId,
        routine: Uuid,
        before: Option<Cursor>,
    },
    RunNow {
        id: Uuid,
        bot: BotId,
        routine: Uuid,
        revision: u32,
    },
    Enable {
        id: Uuid,
        bot: BotId,
        routine: Uuid,
        revision: u32,
        enabled: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineOccurrenceCursor")]
pub struct Cursor {
    pub at_ms: i64,
    pub id: Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineOccurrence")]
pub struct Occurrence {
    pub id: Uuid,
    pub routine: Uuid,
    pub definition_revision: u32,
    pub occurrence_ms: i64,
    pub manual: bool,
    pub state: String,
    pub reason: Option<String>,
    pub message: Option<Uuid>,
    pub reply: Option<Uuid>,
    pub card: Option<Uuid>,
    pub recorded_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<koru_ai_core::bots::BotReplyPreview>,
    #[serde(default)]
    pub review: Option<String>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineSnapshot")]
pub struct Snapshot {
    pub detail: BotRoutineDetail,
    pub thread: ChatId,
    pub selected_pages: Vec<BotPage>,
    pub occurrences: Vec<Occurrence>,
    pub next: Option<Cursor>,
}
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineSnapshot(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(rename = "BotRoutineResponse")]
pub enum Response {
    Created {
        request: Uuid,
        item: koru_ai_core::bots::BotRoutineListItem,
        first_enabled: bool,
        replayed: bool,
    },
    Reviews {
        page: ReviewPage,
    },
    Review {
        detail: ReviewDetail,
    },
    SkippedReview {
        request: Uuid,
        item: ReviewItem,
        replayed: bool,
    },
    Startup {
        request: Uuid,
        first_enabled: bool,
        replayed: bool,
    },
    History {
        snapshot: Snapshot,
    },
    Started {
        request: Uuid,
        occurrence: Occurrence,
        replayed: bool,
    },
    Enabled {
        request: Uuid,
        control: BotRoutineControl,
        first_enabled: bool,
        replayed: bool,
    },
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineResponse(<redacted>)")
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineReviewItem")]
pub struct ReviewItem {
    pub occurrence: Uuid,
    pub routine: Uuid,
    pub name: String,
    pub card: Uuid,
    pub reply: Uuid,
    pub published_at_ms: i64,
    pub review: Option<String>,
    pub preview: String,
    pub truncated: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineReviewPage")]
pub struct ReviewPage {
    pub bot: Uuid,
    pub count: u64,
    pub items: Vec<ReviewItem>,
    pub next: Option<Cursor>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotRoutineReviewDetail")]
pub struct ReviewDetail {
    pub bot: Uuid,
    pub item: ReviewItem,
    pub text: String,
    pub offset: usize,
    pub next: Option<usize>,
}
