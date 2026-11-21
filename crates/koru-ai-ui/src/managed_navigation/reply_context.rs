// SPDX-License-Identifier: MPL-2.0
//! Reply links are derived from retained native card identities, never copied
//! into the person's prose. Reopening always fetches the original result.
use super::*;

pub(super) fn rows(state: &managed::State, lang: koru_i18n::Lang) -> Vec<Row> {
    let label = &state.snapshot.conversation.label;
    if label.purpose != koru_ai_core::bots::BotChatPurpose::Main {
        return Vec::new();
    }
    state
        .messages
        .values()
        .filter_map(|message| {
            if message.kind != "words" || !message.sender.starts_with("person:") {
                return None;
            }
            let anchor = message.content.in_reply_to?;
            let card = state
                .navigation
                .items
                .iter()
                .find(|card| card.id == anchor)?;
            if card.bot != Some(label.bot_id) {
                return None;
            }
            let result = card.routine.as_ref()?;
            let heading = result
                .preview
                .as_ref()
                .filter(|p| p.message.as_uuid() == result.reply)
                .and_then(|p| p.markdown.lines().find(|line| !line.trim().is_empty()))
                .unwrap_or(&card.title)
                .trim()
                .to_owned();
            Some(Row {
                id: message.id,
                heading: if heading.is_empty() {
                    t(lang, Words::BotsRoutine).into()
                } else {
                    heading
                },
                body: String::new(),
                label: String::new(),
                action: Some(Action::Review(label.bot_id, result.occurrence)),
                before: Some(MessageId::from_uuid(message.id)),
                after: None,
                top: f32::NAN,
                height: 0.0,
                targets: Vec::new(),
                replies: Vec::new(),
                routine: false,
            })
        })
        .collect()
}

pub(super) fn tree(entry: &Row, engine: &mut TextEngine, width: f32) -> koru_draw::el::El {
    use koru_draw::el::{Action as ElAction, Key, Length, Role, icon, row, spacer, text};
    use koru_draw::theme::{IconSize, Ink, Profile, Space, roles};
    let available = (width - Space::S2.px(Profile::Desktop) * 2.0).max(1.0);
    let measured = engine.simple_layout(&entry.heading, role::meta()).width();
    row("Bot message thread")
        .pad_x(Space::S2)
        .pad_y(Space::S1)
        .child(spacer())
        .child(
            row("Routine reply context")
                .w(Length::Runtime(
                    (measured + 24.0).min(available),
                    "bounded reply caption",
                ))
                .gap(Space::S1)
                .items_center()
                .id(Key::model(entry.id.as_u128(), "bot-thread"))
                .child(
                    icon(icons::KEY_RETURN, IconSize::XS)
                        .decorative()
                        .ink(Ink::Secondary),
                )
                .child(
                    text(&entry.heading)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .fill()
                        .truncate(),
                )
                .hit()
                .focusable()
                .action(ElAction::Click)
                .semantics(Role::Button)
                .name_text(&entry.heading)
                .states(&THREAD_STATES),
        )
}

#[cfg(test)]
mod tests;
