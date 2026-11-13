// SPDX-License-Identifier: MPL-2.0

//! Ordinary Memory controls composed into the existing Library card.
use super::region::card_title;
use super::*;
use crate::bots::memory::{Notice, Request};
use koru_ai_core::bots::memory::BOT_MEMORY_AVAILABLE;
use koru_ai_core::bots::{
    BotMemoryDetailPage, BotMemoryListPage, BotMemoryNodeId, BotMemoryState, BotMemoryStatusState,
};
use koru_draw::el::{El, Key, Role, col, row};
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::button::ActionStyle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Refresh,
    Create,
    RetryEnsure,
    Retry,
    Abandon,
    Open(Option<BotMemoryNodeId>),
    Facts(bool),
    Detail(BotMemoryNodeId, bool),
    Back,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub request: Request,
    pub focus: Key,
}

impl BotView {
    #[cfg(test)]
    pub(crate) fn enable_memory_for_test(&mut self) {
        self.memory_test_available = true;
    }
    pub(super) fn memory_available(&self) -> bool {
        #[cfg(test)]
        if self.memory_test_available {
            return true;
        }
        BOT_MEMORY_AVAILABLE
    }
    pub(super) fn memory_key(&self, part: &'static str) -> Key {
        Key::model(self.bot.map_or(0, |bot| bot.as_uuid().as_u128()), part)
    }
    pub(super) fn memory_pass(&mut self) -> bool {
        if !self.memory_available() {
            return false;
        }
        self.memory.show(
            !self.stale
                && self.settings_open
                && self.create.is_none()
                && !matches!(self.load, Load::Denied),
        );
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        let Some(request) = self.memory.read(self.context_instance, bot) else {
            return false;
        };
        self.send_memory(request);
        true
    }
    fn send_memory(&mut self, request: Request) {
        let sent = self
            .link
            .as_ref()
            .is_some_and(|link| link.send(Command::Memory(request.clone())));
        let mut work = self.work.get();
        work.commands += usize::from(sent);
        self.work.set(work);
        if !sent {
            self.memory.unsent(&request);
        }
    }
    pub(super) fn memory_section(&self, engine: &mut TextEngine) -> El {
        let bot = self.settings.bot().expect("existing Bot Settings");
        let busy = self.memory.busy();
        let current = self.memory.current(bot);
        let editable = !self.settings.delivering();
        let mut body = col("Memory section")
            .id(self.memory_key("memory-section"))
            .semantics(Role::Group)
            .name(Words::BotsMemory)
            .gap(Space::S2)
            .child(card_title(t(self.lang, Words::BotsMemory)))
            .child(line(
                "Memory explanation",
                self.memory_key("memory-hint"),
                self.words(Words::BotsMemoryHint),
                Ink::Secondary,
            ));
        let words = if busy {
            Words::BotsLoading
        } else {
            match self.memory.status.as_ref().map(|s| &s.state) {
                None => Words::BotsUnavailable,
                Some(BotMemoryStatusState::NotStarted { .. }) => Words::BotsMemoryNotStarted,
                Some(BotMemoryStatusState::Candidate { state, .. }) => match state {
                    BotMemoryState::Prepared => Words::BotsMemoryPrepared,
                    BotMemoryState::Unconfirmed => Words::BotsMemoryUnconfirmed,
                    BotMemoryState::Conflict => Words::BotsMemoryConflict,
                    BotMemoryState::Abandoned => Words::BotsAbandoned,
                    BotMemoryState::Bound => Words::BotsMemoryBound,
                },
            }
        };
        body = body.child(line(
            "Memory status",
            self.memory_key("memory-status"),
            self.words(words),
            Ink::Secondary,
        ));
        if self.memory.status.is_some() && !current {
            body = body.child(line(
                "Memory stale status",
                self.memory_key("memory-stale"),
                self.words(Words::BotsMemoryStale),
                Ink::Secondary,
            ));
        }
        if let Some(notice) = self.memory.notice {
            let words = match notice {
                Notice::Unavailable => Words::BotsErrUnreachable,
                Notice::Unknown => Words::BotsMemoryUnknown,
                Notice::Reconcile => Words::BotsMemoryReconcile,
                Notice::Refused => Words::BotsErrNotApplied,
                Notice::Expired => Words::BotsMemoryExpired,
                Notice::Unsupported => Words::BotsMemoryUnsupported,
                Notice::NotFound => Words::BotsMemoryNotFound,
            };
            body = body.child(line(
                "Memory notice",
                self.memory_key("memory-notice"),
                self.words(words),
                Ink::Accent,
            ));
        }
        // Separate rows keep translations inside the shared form at narrow widths.
        let actions = [
            ("memory-check", Words::BotsMemoryCheck, true, busy),
            (
                "memory-create",
                Words::BotsMemoryCreate,
                matches!(
                    self.memory.status.as_ref().map(|s| &s.state),
                    Some(BotMemoryStatusState::NotStarted { .. })
                ) && !self.memory.uncertain_ensure()
                    && current
                    && !busy,
                !editable || !self.memory.can_create(bot),
            ),
            (
                "memory-retry-request",
                Words::BotsMemoryRetryRequest,
                self.memory.uncertain_ensure(),
                busy || !editable
                    || bot.lifecycle != BotLifecycle::Active
                    || bot.pending_change.is_some()
                    || bot.held_by.is_some(),
            ),
            (
                "memory-retry",
                Words::BotsRetry,
                self.memory.page().is_some() && !self.memory.bound(),
                !editable || !self.memory.can_retry(bot),
            ),
            (
                "memory-abandon",
                Words::BotsAbandon,
                self.memory.page().is_some() && !self.memory.bound(),
                !editable || !self.memory.can_abandon(bot),
            ),
            (
                "memory-open",
                Words::BotsMemoryOpen,
                self.memory.page().is_some(),
                false,
            ),
            (
                "memory-facts",
                Words::BotsMemoryFacts,
                self.memory.bound(),
                busy || !current,
            ),
        ];
        for (part, words, shown, disabled) in actions {
            if shown {
                body = body.child(row("Memory action").child(self.button(
                    engine,
                    "Memory action button",
                    self.memory_key(part),
                    self.words(words),
                    ActionStyle::Secondary,
                    disabled,
                )));
            }
        }
        if let Some(detail) = &self.memory.detail {
            body = body.child(self.memory_detail(engine, detail));
        } else if let Some(list) = &self.memory.list {
            body = body.child(self.memory_list(engine, list));
        }
        body
    }
    fn memory_list(&self, engine: &mut TextEngine, page: &BotMemoryListPage) -> El {
        let mut body = col("Memory fact snapshot")
            .id(self.memory_key("memory-facts-body"))
            .gap(Space::S2);
        match page {
            BotMemoryListPage::Absent {} => body.child(line(
                "Memory absent",
                self.memory_key("memory-absent"),
                self.words(Words::BotsMemoryAbsent),
                Ink::Secondary,
            )),
            BotMemoryListPage::Observed {
                counts,
                items,
                continuation,
                ..
            } => {
                body = body.child(line(
                    "Memory snapshot",
                    self.memory_key("memory-snapshot"),
                    self.words(if self.memory.list_stale {
                        Words::BotsMemoryStale
                    } else {
                        Words::BotsMemorySnapshot
                    }),
                    Ink::Secondary,
                ));
                let values = [
                    counts.valid,
                    counts.omitted_from_prompt,
                    counts.invalid,
                    counts.not_indexed,
                ]
                .map(|n| n.to_string());
                body = body.child(line(
                    "Memory counts",
                    self.memory_key("memory-counts"),
                    fill(
                        t(self.lang, Words::BotsMemoryCounts),
                        &values.iter().map(String::as_str).collect::<Vec<_>>(),
                    ),
                    Ink::Secondary,
                ));
                if counts.valid == 0 && counts.invalid == 0 && counts.not_indexed == 0 {
                    body = body.child(line(
                        "Memory empty",
                        self.memory_key("memory-empty"),
                        self.words(Words::BotsMemoryEmpty),
                        Ink::Secondary,
                    ));
                }
                for item in items {
                    let id = item.fact_id.as_uuid().as_u128();
                    body = body.child(
                        col("Memory fact")
                            .id(Key::model(id, "memory-fact"))
                            .semantics(Role::Group)
                            .name_text(item.summary.clone())
                            .gap(Space::S1)
                            .child(line(
                                "Memory fact summary",
                                Key::model(id, "memory-fact-summary"),
                                item.summary.clone(),
                                Ink::Primary,
                            ))
                            .child(
                                self.button(
                                    engine,
                                    "Read Memory fact",
                                    Key::model(id, "memory-detail"),
                                    self.words(Words::BotsMemoryDetails),
                                    ActionStyle::Secondary,
                                    self.memory.busy()
                                        || !self
                                            .settings
                                            .bot()
                                            .is_some_and(|bot| self.memory.current(bot)),
                                ),
                            )
                            .child(self.button(
                                engine,
                                "Open live Memory fact",
                                Key::model(id, "memory-fact-open"),
                                self.words(Words::BotsMemoryOpen),
                                ActionStyle::Ghost,
                                false,
                            )),
                    );
                }
                if continuation.is_some() {
                    body = body.child(self.button(
                        engine,
                        "Next Memory facts",
                        self.memory_key("memory-next"),
                        self.words(Words::BotsMemoryNext),
                        ActionStyle::Secondary,
                        self.memory.busy() || self.memory.list_stale,
                    ));
                }
                body.child(self.button(
                    engine,
                    "New Memory snapshot",
                    self.memory_key("memory-start"),
                    self.words(Words::BotsMemoryStartOver),
                    ActionStyle::Ghost,
                    self.memory.busy(),
                ))
            }
        }
    }
    fn memory_detail(&self, engine: &mut TextEngine, page: &BotMemoryDetailPage) -> El {
        let mut body = col("Memory fact detail")
            .id(self.memory_key("memory-detail-body"))
            .gap(Space::S2)
            .child(line(
                "Memory detail snapshot",
                self.memory_key("memory-detail-snapshot"),
                self.words(if self.memory.detail_stale {
                    Words::BotsMemoryStale
                } else {
                    Words::BotsMemoryDetailSnapshot
                }),
                Ink::Secondary,
            ));
        match page {
            BotMemoryDetailPage::Absent {} => {
                body = body.child(line(
                    "Memory absent",
                    self.memory_key("memory-detail-absent"),
                    self.words(Words::BotsMemoryAbsent),
                    Ink::Secondary,
                ));
            }
            BotMemoryDetailPage::Observed {
                chunks,
                continuation,
                ..
            } => {
                for (index, chunk) in chunks.iter().enumerate() {
                    let mut identity = self.memory.body_serial.to_be_bytes().to_vec();
                    identity.extend_from_slice(&(index as u64).to_be_bytes());
                    if let Some(range) = &chunk.range {
                        identity.extend_from_slice(&range.start_utf8.to_be_bytes());
                        identity.extend_from_slice(&range.end_utf8.to_be_bytes());
                    }
                    let id = uuid::Uuid::new_v5(&chunk.node_id.as_uuid(), &identity);
                    let key = Key::model(id.as_u128(), "memory-chunk");
                    let mut group = col("Memory detail chunk")
                        .id(key)
                        .semantics(Role::Group)
                        .name(Words::BotsMemoryDetails)
                        .gap(Space::S1);
                    if chunk
                        .range
                        .as_ref()
                        .is_some_and(|range| range.start_utf8 > 0)
                    {
                        group = group.child(line(
                            "Continued Memory text",
                            Key::model(id.as_u128(), "memory-continued"),
                            self.words(Words::BotsMemoryContinued),
                            Ink::Secondary,
                        ));
                    }
                    match excerpt(&chunk.content) {
                        Some(lines) => {
                            for (n, (source, value)) in lines.into_iter().enumerate() {
                                let value = if source {
                                    fill(t(self.lang, Words::BotsMemorySource), &[value.as_str()])
                                } else {
                                    value
                                };
                                group = group.child(line(
                                    "Memory detail text or source",
                                    Key::model(
                                        uuid::Uuid::new_v5(&id, &(n as u64).to_be_bytes())
                                            .as_u128(),
                                        "memory-chunk-value",
                                    ),
                                    value,
                                    Ink::Primary,
                                ));
                            }
                        }
                        None => {
                            group = group.child(line(
                                "Unsupported Memory detail",
                                Key::model(id.as_u128(), "memory-unsupported"),
                                self.words(Words::BotsMemoryUnsupported),
                                Ink::Accent,
                            ));
                        }
                    }
                    body = body.child(group);
                }
                if continuation.is_some() {
                    body = body.child(self.button(
                        engine,
                        "Next Memory detail",
                        self.memory_key("memory-detail-next"),
                        self.words(Words::BotsMemoryNext),
                        ActionStyle::Secondary,
                        self.memory.busy() || self.memory.detail_stale,
                    ));
                }
            }
        }
        body.child(self.button(
            engine,
            "Back to Memory facts",
            self.memory_key("memory-back"),
            self.words(Words::Back),
            ActionStyle::Ghost,
            false,
        ))
    }
    pub(super) fn memory_action(&self, key: Key) -> Option<Action> {
        if !self.memory_available() || !self.settings_open {
            return None;
        }
        let standing = if key.object == self.memory_key(key.part).object {
            match key.part {
                "memory-check" => Some(Action::Refresh),
                "memory-create" => Some(Action::Create),
                "memory-retry-request" => Some(Action::RetryEnsure),
                "memory-retry" => Some(Action::Retry),
                "memory-abandon" => Some(Action::Abandon),
                "memory-open" => Some(Action::Open(None)),
                "memory-facts" | "memory-start" => Some(Action::Facts(false)),
                "memory-next" => Some(Action::Facts(true)),
                "memory-back" => Some(Action::Back),
                "memory-detail-next" => match &self.memory.detail {
                    Some(BotMemoryDetailPage::Observed { fact_id, .. }) => {
                        Some(Action::Detail(*fact_id, true))
                    }
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        standing.or_else(|| {
            let Some(BotMemoryListPage::Observed { items, .. }) = &self.memory.list else {
                return None;
            };
            let fact = items
                .iter()
                .find(|item| Key::model(item.fact_id.as_uuid().as_u128(), key.part) == key)?;
            match key.part {
                "memory-detail" => Some(Action::Detail(fact.fact_id, false)),
                "memory-fact-open" => Some(Action::Open(Some(fact.fact_id))),
                _ => None,
            }
        })
    }
    pub(super) fn perform_memory(&mut self, action: Action) -> bool {
        if !self.memory_available() || self.stale || !self.settings_open || self.create.is_some() {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        if bot.lifecycle == BotLifecycle::Trashed || self.load == Load::Denied {
            return false;
        }
        if self.settings.delivering()
            && matches!(
                action,
                Action::Create | Action::RetryEnsure | Action::Retry | Action::Abandon
            )
        {
            return false;
        }
        let request = match action {
            Action::Refresh => {
                self.memory.refresh();
                self.asked = false;
                return true;
            }
            Action::Create => self.memory.create(
                self.context_instance,
                bot,
                t(self.lang, Words::BotsMemory).to_owned(),
            ),
            Action::RetryEnsure => self.memory.retry_ensure(self.context_instance, bot),
            Action::Retry => self.memory.retry(self.context_instance, bot),
            Action::Abandon => {
                let Some(request) = self.memory.ask_abandon(self.context_instance, bot) else {
                    return false;
                };
                self.memory_ask = Some(Box::new(Ask {
                    request,
                    focus: self.memory_key("memory-abandon"),
                }));
                return true;
            }
            Action::Open(node) => {
                if let Some(id) = node {
                    let listed = matches!(&self.memory.list, Some(BotMemoryListPage::Observed { items, .. }) if items.iter().any(|item| item.fact_id == id));
                    let detailed = matches!(&self.memory.detail, Some(BotMemoryDetailPage::Observed { fact_id, .. }) if *fact_id == id);
                    if !listed && !detailed {
                        return false;
                    }
                }
                let Some(page) = self.memory.page() else {
                    return false;
                };
                self.memory_target = Some(Box::new(koru_editor::trigger::DocumentTarget {
                    document: koru_core::DocumentId::from_uuid(page.document.as_uuid()),
                    node: node.map(|id| koru_core::NodeId::from_uuid(id.as_uuid())),
                }));
                return true;
            }
            Action::Facts(next) => self.memory.facts(self.context_instance, bot, next),
            Action::Detail(id, next) => self.memory.fact(self.context_instance, bot, id, next),
            Action::Back => {
                let focus = match &self.memory.detail {
                    Some(BotMemoryDetailPage::Observed { fact_id, .. }) => {
                        Some(Key::model(fact_id.as_uuid().as_u128(), "memory-detail"))
                    }
                    _ => None,
                };
                let changed = self.memory.back();
                self.focus = focus.or(Some(self.memory_key("memory-check")));
                self.reveal_focus();
                return changed;
            }
        };
        if let Some(request) = request {
            self.send_memory(request);
            true
        } else {
            false
        }
    }
    pub(crate) fn take_memory_ask(&mut self) -> Option<Ask> {
        self.memory_ask.take().map(|ask| *ask)
    }
    pub(crate) fn settle_memory_ask(&mut self, ask: &Ask, confirmed: bool) -> bool {
        if !self.memory_available()
            || ask.request.instance != self.context_instance
            || Some(ask.request.bot) != self.bot
        {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        if let Some(request) =
            self.memory
                .confirm(self.context_instance, bot, &ask.request, confirmed)
        {
            self.send_memory(request);
        }
        self.focus = Some(
            if self
                .settings
                .bot()
                .is_some_and(|bot| self.memory.can_abandon(bot))
            {
                ask.focus
            } else {
                self.memory_key("memory-check")
            },
        );
        self.reveal_focus();
        true
    }
    pub(crate) fn take_document_request(&mut self) -> Option<koru_editor::trigger::DocumentTarget> {
        self.memory_target.take().map(|target| *target).or_else(|| {
            self.take_role_request()
                .map(|page| koru_editor::trigger::DocumentTarget {
                    document: koru_core::DocumentId::from_uuid(page.document.as_uuid()),
                    node: None,
                })
        })
    }
}

/// Literal excerpt plus complete typed source values; no Markdown, lookup or dereference.
fn line(name: &'static str, key: Key, words: String, ink: Ink) -> El {
    super::region::line(name, key, words.clone(), ink).name_text(words)
}

fn excerpt(content: &serde_json::Value) -> Option<Vec<(bool, String)>> {
    let mut rows = Vec::new();
    match content.get("type")?.as_str()? {
        "text" => {
            rows.push((false, content.get("text")?.as_str()?.to_owned()));
            content.get("runs")?.as_array()?;
        }
        "blob" => {
            rows.push((
                true,
                format!(
                    "{} · {}",
                    content.get("mime")?.as_str()?,
                    content.get("hash")?.as_str()?
                ),
            ));
        }
        "embed" => {
            rows.push((true, serde_json::to_string(content.get("target")?).ok()?));
            return Some(rows);
        }
        _ => return None,
    }
    for attr in content.get("attributes")?.as_array()? {
        let value = attr.get("value")?;
        let kind = value.get("type")?.as_str()?;
        if !matches!(
            kind,
            "date"
                | "document"
                | "ref"
                | "url"
                | "text"
                | "datetime_floating"
                | "datetime_zoned"
                | "number"
                | "bool"
        ) {
            return None;
        }
        let label = attr
            .get("label")?
            .as_str()
            .or_else(|| attr.get("role")?.as_str())
            .unwrap_or("");
        let shown = match kind {
            "url" | "text" | "date" => value.get("value")?.as_str()?.to_owned(),
            _ => serde_json::to_string(value).ok()?,
        };
        rows.push((
            true,
            if label.is_empty() {
                shown
            } else {
                format!("{label}: {shown}")
            },
        ));
    }
    Some(rows)
}
