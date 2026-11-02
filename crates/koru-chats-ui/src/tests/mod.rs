// SPDX-License-Identifier: MPL-2.0

//! The surface's deterministic states, driven without a gateway.
//!
//! Every test pushes a [`Listing`] in through
//! [`ChatsSurface::take_listing`] — the seam the reader uses — so nothing here
//! dials a service, opens a socket or touches a disk. `dependency` pins that.

use koru_ai_core::{AiProviderId, Chat, ChatId, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_wire::AgentProviderView;
use koru_draw::geometry::Rect;
use uuid::Uuid;

use crate::{ChatsSurface, Listing, Row};

mod dependency;
mod freshness;
mod keys;
mod locales;
mod rows;

/// The list as the navigator hands it over: 260 wide, rows starting a little
/// under the mode row.
pub(crate) const TOP: f32 = 80.0;

pub(crate) fn area() -> Rect {
    Rect::new(0.0, 0.0, 260.0, 720.0)
}

/// A tall list, so a test that means to see every row is not looking at the
/// cull.
pub(crate) fn tall() -> Rect {
    Rect::new(0.0, 0.0, 260.0, 100_000.0)
}

/// The clock every relative time in these tests is read against.
pub(crate) const NOW: i64 = 10_000_000_000;

pub(crate) fn id(n: u32) -> ChatId {
    ChatId::from_uuid(
        format!("{n:08x}-0000-4000-8000-000000000000")
            .parse::<Uuid>()
            .unwrap(),
    )
}

pub(crate) fn model(provider: &str, requested: &str) -> ModelSelection {
    ModelSelection {
        provider: AiProviderId::parse(provider).unwrap(),
        requested_model: requested.into(),
        default_reasoning: ReasoningChoice::Default,
    }
}

/// A chat last touched `ago_ms` before [`NOW`].
pub(crate) fn chat(n: u32, title: &str, ago_ms: i64) -> Chat {
    Chat {
        id: id(n),
        title: title.into(),
        private: false,
        current_model: model("openai", "gpt-6-astra"),
        branched_from: None,
        agent_working_folder: None,
        agent_mode: None,
        agent_fast: None,
        created_at: Timestamp::from_millis(NOW - ago_ms - 1_000),
        updated_at: Timestamp::from_millis(NOW - ago_ms),
    }
}

pub(crate) fn opencode() -> AgentProviderView {
    AgentProviderView {
        id: AiProviderId::parse("agent-opencode").unwrap(),
        label: "OpenCode".into(),
    }
}

/// A surface showing `chats`, with a fixed clock.
pub(crate) fn showing(chats: Vec<Chat>) -> ChatsSurface {
    let mut surface = ChatsSurface::without_reader();
    surface.set_now(NOW);
    surface.take_listing(Ok(Listing {
        chats,
        agents: vec![opencode()],
    }));
    surface
}

/// Three chats, deliberately handed over out of order.
pub(crate) fn three() -> ChatsSurface {
    showing(vec![
        chat(2, "Middle", 60_000),
        chat(3, "Oldest", 3_600_000),
        chat(1, "Newest", 1_000),
    ])
}

/// Every row, ignoring the band.
pub(crate) fn rows_of(surface: &mut ChatsSurface) -> Vec<Row> {
    surface
        .rows(tall(), TOP)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

pub(crate) fn titles(surface: &ChatsSurface) -> Vec<String> {
    surface
        .chats()
        .iter()
        .map(|chat| chat.title.clone())
        .collect()
}
