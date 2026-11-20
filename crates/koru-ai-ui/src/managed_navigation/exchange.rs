// SPDX-License-Identifier: MPL-2.0

//! Quiet collaboration receipts, grouped by the engine's originating turn.
//! Targets retain their own thread links: two private exchanges are not a
//! fabricated shared conversation.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct Target {
    pub bot: koru_ai_core::bots::BotId,
    pub name: String,
    pub owner: koru_ai_core::bots::BotId,
    pub owner_name: String,
    pub chat: ChatId,
}

pub(super) fn target_key(target: &Target) -> koru_draw::el::Key {
    koru_draw::el::Key::model(target.chat.as_uuid().as_u128(), "exchange-target")
}

pub(super) fn rows(
    cards: &[wire::Card],
    layouts: &[MessageLayout],
    lang: koru_i18n::Lang,
) -> Vec<Row> {
    let mut groups: BTreeMap<Uuid, Vec<(&wire::Card, &wire::Dispatch)>> = BTreeMap::new();
    for card in cards {
        for dispatch in &card.dispatches {
            let group = groups.entry(dispatch.group).or_default();
            if !group.iter().any(|(_, held)| {
                held.message == dispatch.message && held.recipient == dispatch.recipient
            }) {
                group.push((card, dispatch));
            }
        }
    }
    let mut rows = Vec::new();
    for (id, dispatches) in groups {
        let Some(after) = dispatches
            .first()
            .and_then(|(_, dispatch)| dispatch.origin_reply)
            .map(MessageId::from_uuid)
            .filter(|id| layouts.iter().any(|message| message.id == Some(*id)))
        else {
            // An unloaded origin must never attach its completed exchange
            // to a different turn at the tail.
            continue;
        };
        let mut targets: Vec<Target> = Vec::new();
        for (card, dispatch) in &dispatches {
            let Some(chat) = card.thread else { continue };
            if !targets
                .iter()
                .any(|t| t.bot == dispatch.recipient && t.chat == chat)
            {
                targets.push(Target {
                    bot: dispatch.recipient,
                    name: dispatch.name.clone(),
                    owner: card.bot.unwrap_or(dispatch.recipient),
                    owner_name: card.bot_name.clone(),
                    chat,
                });
            }
        }
        let replied = dispatches.iter().filter(|(_, d)| d.reply.is_some()).count();
        let ended = dispatches
            .iter()
            .filter(|(_, d)| {
                d.reply.is_none() && matches!(d.state.as_str(), "expired" | "cancelled")
            })
            .count();
        if let Some(mut row) = summary(id, targets, replied, ended, dispatches.len(), None, lang) {
            row.after = Some(after);
            for (card, dispatch) in &dispatches {
                if let Some(preview) = dispatch
                    .preview
                    .as_ref()
                    .filter(|preview| dispatch.reply == Some(preview.message.as_uuid()))
                    && let Some(target) = row.targets.iter().find(|target| {
                        target.bot == dispatch.recipient && Some(target.chat) == card.thread
                    })
                    && !row
                        .replies
                        .iter()
                        .any(|reply| reply.preview.message == preview.message)
                {
                    row.replies
                        .push(reply::Reply::new(target.clone(), preview.clone()));
                }
            }
            rows.push(row);
        }
    }
    rows
}

fn summary(
    id: Uuid,
    targets: Vec<Target>,
    replied: usize,
    ended: usize,
    total: usize,
    before: Option<MessageId>,
    lang: koru_i18n::Lang,
) -> Option<Row> {
    let first = targets.first()?;
    let mut body = t(lang, Words::BotsExchangeSent).to_owned();
    if replied > 0 {
        body.push_str(" · ");
        body.push_str(&koru_i18n::fill(
            t(lang, Words::BotsExchangeReplies),
            &[&replied.to_string(), &total.to_string()],
        ));
    }
    if ended > 0 {
        body.push_str(" · ");
        body.push_str(&koru_i18n::fill(
            t(lang, Words::BotsExchangeUnanswered),
            &[&ended.to_string()],
        ));
    }
    Some(Row {
        id,
        heading: targets
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        body,
        label: t(lang, Words::BotsChildOpenConversation).into(),
        action: Some(Action::Thread(
            first.owner,
            first.owner_name.clone(),
            first.chat,
        )),
        before,
        after: None,
        top: f32::NAN,
        height: 0.0,
        targets,
        replies: Vec::new(),
        routine: false,
    })
}

pub(super) fn tree(
    entry: &Row,
    engine: &mut TextEngine,
    faces: &HashMap<Uuid, u8>,
    width: f32,
) -> koru_draw::el::El {
    use koru_draw::el::{Action as ElAction, Length, Role, col, leaf, row, spacer, text};
    use koru_draw::theme::{Ink, Profile, Space, roles};
    let gap = Space::S2.px(Profile::Desktop);
    let available = (width - gap * 2.0).max(1.0);
    let mut lines = col("Bot message thread").pad_y(Space::S1).gap(Space::S1);
    // Center bounded contents, including on narrow windows. Participant chips
    // wrap as complete identities rather than overflowing the transcript.
    let mut line = row("Exchange targets").gap(Space::S2).items_center();
    let mut used = 0.0;
    if !entry.body.is_empty() {
        used = engine
            .simple_layout(&entry.body, koru_draw::theme::role::meta())
            .width()
            .min(available);
        line = line.child(
            text(&entry.body)
                .role(roles::META)
                .ink(Ink::Secondary)
                .w(Length::Runtime(used, "measured exchange status"))
                .wrap(),
        );
    }
    for target in &entry.targets {
        use koru_draw::el::Leaf;
        let chip = chip::BotChip::bounded(
            engine,
            faces.get(&target.bot.as_uuid()).copied().unwrap_or(0),
            &target.name,
            available,
        );
        let chip_width = chip.measure(None).0.min(available);
        if used > 0.0 && used + gap + chip_width > available {
            lines = lines.child(
                row("Centered targets")
                    .child(spacer())
                    .child(line)
                    .child(spacer()),
            );
            line = row("Exchange targets").gap(Space::S2).items_center();
            used = 0.0;
        }
        let identity = leaf("Bot identity", chip)
            .semantic()
            .w(Length::Runtime(chip_width, "measured participant name"));
        line = line.child(if entry.action.is_some() {
            identity
                .id(target_key(target))
                .hit()
                .focusable()
                .action(ElAction::Click)
                .semantics(Role::Button)
                .name_text(&target.name)
                .states(&THREAD_STATES)
        } else {
            identity.semantics(Role::Image).name_text(&target.name)
        });
        used += if used == 0.0 {
            chip_width
        } else {
            gap + chip_width
        };
    }
    lines = lines.child(
        row("Centered targets")
            .child(spacer())
            .child(line)
            .child(spacer()),
    );
    for reply in &entry.replies {
        lines = lines.child(reply::tree(reply, engine, faces, width, &entry.label));
    }
    lines
}
