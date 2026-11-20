// SPDX-License-Identifier: MPL-2.0
//! One durable Routine result per occurrence, with its exact returned words.
//! The shared reply renderer owns the Markdown; the link opens its thread.
use super::*;

pub(super) fn row(
    card: &wire::Card,
    before: Option<MessageId>,
    lang: koru_i18n::Lang,
) -> Option<Row> {
    let result = card.routine.as_ref()?;
    let bot = card.bot?;
    let chat = card.thread?;
    let target = exchange::Target {
        bot,
        name: card.bot_name.clone(),
        owner: bot,
        owner_name: card.bot_name.clone(),
        chat,
    };
    let replies = result
        .preview
        .as_ref()
        .filter(|preview| preview.message.as_uuid() == result.reply)
        .map(|preview| reply::Reply::new(target, preview.clone()))
        .into_iter()
        .collect();
    Some(Row {
        // Card identity, not thread identity: each firing keeps its own place.
        id: card.id,
        heading: card.title.clone(),
        body: t(
            lang,
            if result.manual {
                Words::BotsRoutineRunNow
            } else {
                Words::BotsRoutine
            },
        )
        .into(),
        label: t(lang, Words::BotsChildOpenConversation).into(),
        action: Some(Action::Thread(bot, card.bot_name.clone(), chat)),
        before,
        after: None,
        top: f32::NAN,
        height: 0.0,
        targets: Vec::new(),
        replies,
        routine: true,
    })
}

pub(super) fn tree(
    entry: &Row,
    engine: &mut TextEngine,
    faces: &HashMap<Uuid, u8>,
    width: f32,
) -> koru_draw::el::El {
    use koru_draw::el::{Action as ElAction, Key, Length, Role, col, row, spacer, text};
    use koru_draw::theme::{Ink, Profile, Space, roles};

    let available = (width - Space::S3.px(Profile::Desktop) * 2.0).max(1.0);
    let heading = if entry.heading.is_empty() {
        entry.body.clone()
    } else {
        format!("{} · {}", entry.body, entry.heading)
    };
    let heading_width = engine
        .simple_layout(&heading, role::meta())
        .width()
        .min(available);
    let mut result = col("Routine result").gap(Space::S1).child(
        row("Routine heading")
            .pad_x(Space::S3)
            .child(spacer())
            .child(
                text(heading)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .w(Length::Runtime(heading_width, "measured Routine heading"))
                    .wrap(),
            )
            .child(spacer()),
    );
    for reply in &entry.replies {
        // Every result has one explicit thread link below. The reply's sender
        // remains a pointer shortcut; no second truncation link is needed.
        result = result.child(reply::tree(reply, engine, faces, width, ""));
    }
    let link_width = engine
        .simple_layout(&entry.label, role::meta())
        .width()
        .min(available);
    result = result.child(
        row("Routine conversation")
            .pad_x(Space::S3)
            .child(spacer())
            .child(
                text(&entry.label)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .w(Length::Runtime(link_width, "measured conversation link"))
                    .wrap()
                    .id(Key::model(entry.id.as_u128(), "bot-thread"))
                    .hit()
                    .focusable()
                    .action(ElAction::Click)
                    .semantics(Role::Button)
                    .name_text(&entry.label)
                    .states(&THREAD_STATES),
            )
            .child(spacer()),
    );
    col("Bot message thread")
        .id(Key::model(entry.id.as_u128(), "routine-result"))
        .pad_y(Space::S1)
        .child(result)
}

#[cfg(test)]
mod tests;
