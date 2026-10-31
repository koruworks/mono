// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant, UNIX_EPOCH};

use koru_ai_core::{AiProviderId, Chat, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_wire::ChatTranscript;

use super::*;
use crate::reader::{Command, Reader, Update};
use crate::{SurfaceMode, message_content_width, usage::message_content_height};

fn agent_transcript(provider: &str) -> ChatTranscript {
    let chat_id = ChatId::new();
    let now = Timestamp::from_millis(1);
    ChatTranscript {
        chat: Chat {
            id: chat_id,
            title: "Agent chat".into(),
            private: false,
            current_model: ModelSelection {
                provider: AiProviderId::parse(provider).unwrap(),
                requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        sources: Vec::new(),
        reasoning_control: None,
        attachments: Vec::new(),
    }
}

fn png(width: u32, height: u32) -> koru_clipboard::PngBitmap {
    let rgba = vec![200; (width * height * 4) as usize];
    koru_clipboard::PngBitmap {
        width,
        height,
        bytes: koru_clipboard::png(width, height, &rgba).unwrap(),
    }
}

fn surface_with_agent_chat() -> (ChatSurface, ChatId, std::sync::mpsc::Receiver<Command>) {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reader, commands, _updates) = Reader::test_pair();
    let transcript = agent_transcript("agent-claude");
    let chat = transcript.chat.id;
    surface.reader = Some(reader);
    surface.transcript = Some(transcript);
    (surface, chat, commands)
}

fn stored(chat: ChatId, draft: &Draft) -> Event {
    Event::Stored {
        chat,
        id: draft.id,
        result: Ok(ChatAttachment {
            id: draft.id,
            message_id: None,
            ordinal: None,
            kind: AttachmentKind::Image,
            source: AttachmentSource::Clipboard,
            file_name: draft.file_name.clone(),
            media_type: "image/png".into(),
            byte_count: draft.byte_count,
            dimensions: draft.size,
            origin: None,
            created_at: Timestamp::from_millis(2),
        }),
    }
}

#[test]
fn a_pasted_screenshot_is_named_like_a_screenshot_file() {
    // 2023-11-14 22:13:20 UTC.
    let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(screenshot_name(at), "screenshot-2023-11-14-221320.png");
    let draft = Draft::pasted(png(4, 2), at);
    assert_eq!(draft.size, Some((4, 2)));
    assert_eq!(draft.state, DraftState::Storing);
    assert!(
        koru_ai_core::attachments::allowed_type(&draft.file_name).is_some(),
        "the name passes the allowlist"
    );
}

#[test]
fn card_tiles_keep_the_pictures_shape_and_wrap() {
    let wide = Tile {
        id: ChatAttachmentId::new(),
        kind: AttachmentKind::Image,
        size: Some((400, 200)),
        name: "wide.png".into(),
    };
    let tall = Tile {
        size: Some((100, 400)),
        ..wide.clone()
    };
    let (rects, height) = card_tiles(&[wide.clone(), tall.clone()], 1_000.0);
    assert_eq!(rects[0].w, CARD_TILE_H * 2.0);
    assert_eq!(
        rects[1].w, CARD_TILE_MIN_W,
        "a tall picture is never a sliver"
    );
    assert_eq!(rects[1].y, 0.0);
    assert_eq!(height, CARD_TILE_H);
    // Narrower than both: the second goes to the next row.
    let (rects, height) = card_tiles(&[wide, tall], 240.0);
    assert_eq!(rects[1].x, 0.0);
    assert_eq!(rects[1].y, CARD_TILE_H + TILE_GAP);
    assert_eq!(height, CARD_TILE_H * 2.0 + TILE_GAP);
    assert_eq!(card_tiles(&[], 100.0).1, 0.0);
}

#[test]
fn a_refusal_reads_in_the_persons_language() {
    let failure = |code: &str| Failure {
        code: code.into(),
        message: String::new(),
        retryable: false,
        details: Default::default(),
    };
    assert_eq!(
        failure_line(&failure(attachment_codes::TOO_LARGE), Lang::En).0,
        "Larger than 20 MB."
    );
    assert_eq!(
        failure_line(&failure("ai_unavailable"), Lang::En).0,
        "Koru AI isn't available."
    );
    assert_eq!(
        failure_line(&failure("something_new"), Lang::De).0,
        koru_i18n::t(Lang::De, Key::AiAttachmentFailed)
    );
}

#[test]
fn a_paste_on_a_chat_that_is_not_an_agents_says_so_and_reads_nothing() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.transcript = Some(agent_transcript(AiProviderId::OPENAI));
    assert!(surface.begin_attachment_paste());
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentsAgentsOnly))
    );
    assert!(!surface.clipboard_reading);
}

#[test]
fn a_draft_widens_the_composer_and_holds_the_send_until_it_is_stored() {
    let (mut surface, chat, commands) = surface_with_agent_chat();
    let plain = surface.composer_rect().h;
    let draft = Draft::pasted(png(8, 4), std::time::SystemTime::now());
    surface.add_draft(chat, draft.clone());
    assert!(surface.has_strip());
    assert_eq!(surface.composer_rect().h, plain + surface.strip_band());
    assert!(surface.composer_text_rect().y > surface.composer_hit_rect.y);

    // Enter while it is being stored: nothing goes, and the line says why.
    assert!(surface.send_composer());
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentAddingOne))
    );
    assert!(!matches!(commands.try_recv(), Ok(Command::Send(_))));

    // Stored: a message of the picture alone is sent, naming it.
    surface.attachment_work.inject(stored(chat, &draft));
    assert!(surface.maintain_attachments());
    assert!(surface.send_composer());
    let request = loop {
        match commands.recv_timeout(Duration::from_millis(200)).unwrap() {
            Command::Send(request) => break request,
            _ => continue,
        }
    };
    assert_eq!(request.attachment_ids, vec![draft.id]);
    assert!(request.markdown.is_empty());
    assert!(!surface.has_strip(), "the strip empties into the sent card");
    assert_eq!(surface.pending_user_tiles, vec![draft.tile()]);
}

#[test]
fn a_failed_draft_is_red_blocks_the_send_and_comes_out_with_its_cross() {
    let (mut surface, chat, _commands) = surface_with_agent_chat();
    let draft = Draft::pasted(png(4, 4), std::time::SystemTime::now());
    surface.add_draft(chat, draft.clone());
    surface.attachment_work.inject(Event::Stored {
        chat,
        id: draft.id,
        result: Err(Failure {
            code: attachment_codes::STORAGE.into(),
            message: String::new(),
            retryable: true,
            details: Default::default(),
        }),
    });
    surface.maintain_attachments();
    assert_eq!(
        surface.strip()[0].state,
        DraftState::Failed { retryable: true }
    );
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentDiskFull))
    );
    assert!(surface.send_composer());
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentsRemoveFailed))
    );

    surface.composer_rect();
    let composer = surface.composer_hit_rect;
    let tile = strip_tiles(
        surface.strip(),
        composer.x + crate::COMPOSER_PAD_X,
        composer.y + crate::COMPOSER_PAD_TOP,
    )[0];
    let cross = remove_button(tile);
    assert_eq!(
        surface.strip_mouse_down(cross.x + cross.w * 0.5, cross.y + cross.h * 0.5),
        Some(true)
    );
    assert!(!surface.has_strip());
}

#[test]
fn the_message_limits_hold_in_the_strip() {
    let (mut surface, chat, _commands) = surface_with_agent_chat();
    for _ in 0..koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE {
        surface.add_draft(chat, Draft::pasted(png(2, 2), std::time::SystemTime::now()));
    }
    surface.add_draft(chat, Draft::pasted(png(2, 2), std::time::SystemTime::now()));
    assert_eq!(
        surface.strip().len(),
        koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE
    );
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentLimit))
    );
}

fn sent_request(commands: &std::sync::mpsc::Receiver<Command>) -> koru_ai_wire::SendTurnRequest {
    loop {
        match commands.recv_timeout(Duration::from_millis(200)).unwrap() {
            Command::Send(request) => return request,
            _ => continue,
        }
    }
}

/// A stored draft and some words, ready to send.
fn ready_to_send(surface: &mut ChatSurface, chat: ChatId) -> Draft {
    let draft = Draft::pasted(png(4, 4), std::time::SystemTime::now());
    surface.add_draft(chat, draft.clone());
    surface.attachment_work.inject(stored(chat, &draft));
    surface.maintain_attachments();
    surface.composer.set_text("look at this");
    draft
}

#[test]
fn a_refused_send_gives_the_message_back_with_the_named_attachment_red() {
    let (mut surface, chat, commands) = surface_with_agent_chat();
    let draft = ready_to_send(&mut surface, chat);
    assert!(surface.send_composer());
    sent_request(&commands);
    assert!(!surface.has_strip());
    assert!(surface.composer.is_blank());
    let mut failure = Failure {
        code: attachment_codes::MISSING.into(),
        message: "This file is no longer on this computer.".into(),
        retryable: false,
        details: Default::default(),
    };
    failure
        .details
        .insert("attachment_id".into(), draft.id.to_storage().into());
    surface.apply_update(Update::SendFailed(failure));
    assert_eq!(surface.composer.text(), "look at this", "the text stays");
    assert_eq!(
        surface.strip()[0].state,
        DraftState::Failed { retryable: false }
    );
    assert!(surface.pending_user.is_none(), "no card is left pending");
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentMissing))
    );
}

#[test]
fn a_send_that_fails_for_any_reason_gives_everything_back() {
    let (mut surface, chat, commands) = surface_with_agent_chat();
    let draft = ready_to_send(&mut surface, chat);
    assert!(surface.send_composer());
    sent_request(&commands);
    surface.apply_update(Update::SendFailed(Failure {
        code: "ai_worker_saturated".into(),
        message: "The AI service is busy. Try again.".into(),
        retryable: true,
        details: Default::default(),
    }));
    assert_eq!(surface.composer.text(), "look at this");
    assert_eq!(surface.strip()[0].id, draft.id);
    assert_eq!(
        surface.strip()[0].state,
        DraftState::Stored,
        "nothing named"
    );
    assert_eq!(
        surface.status.as_deref(),
        Some("The AI service is busy. Try again.")
    );

    // Sent again and accepted, nothing is held back any more.
    assert!(surface.send_composer());
    let request = sent_request(&commands);
    assert_eq!(request.attachment_ids, vec![draft.id]);
    surface.apply_update(Update::Accepted(koru_ai_wire::SendTurnAccepted {
        attempt_id: koru_ai_core::InferenceAttemptId::new(),
        user_message_id: koru_ai_core::MessageId::new(),
        assistant_message_id: koru_ai_core::MessageId::new(),
        replayed: false,
    }));
    assert!(surface.sent_message.is_none());
}

#[test]
fn a_send_the_queue_will_not_take_puts_the_message_back() {
    let (mut surface, chat, _commands) = surface_with_agent_chat();
    let draft = ready_to_send(&mut surface, chat);
    // Fill the reader's queue with commands nobody is reading.
    while surface.queue_command(Command::Read(chat)) {}
    assert!(surface.send_composer());
    assert_eq!(surface.composer.text(), "look at this");
    assert_eq!(surface.strip()[0].id, draft.id);
    assert!(surface.pending_user.is_none());
    assert!(surface.sent_message.is_none());
}

#[test]
fn a_paste_with_the_picker_open_is_answered_at_once() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.transcript = Some(agent_transcript(AiProviderId::OPENAI));
    surface.composer.arm_paste_attachment();
    surface.composer_key(
        &winit::keyboard::Key::Character("a".into()),
        Some("a"),
        Default::default(),
    );
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(surface.lang, Key::AiAttachmentsAgentsOnly))
    );
    surface.status = None;
    surface.composer_key(
        &winit::keyboard::Key::Character("b".into()),
        Some("b"),
        Default::default(),
    );
    assert_eq!(
        surface.status, None,
        "nothing was left armed for a later key"
    );
}

#[test]
fn the_thumbnail_cache_keeps_the_most_recently_drawn_within_its_bound() {
    let mut cache = ThumbCache::default();
    let picture = || decode_thumbnail(&png(512, 512).bytes).unwrap();
    let ids: Vec<_> = (0..70).map(|_| ChatAttachmentId::new()).collect();
    for id in &ids {
        cache.ready(*id, picture());
        // The first stays in use, so it is never the one to go.
        assert!(cache.get(ids[0]).is_some());
    }
    assert!(cache.bytes() <= THUMB_CACHE_BYTES);
    assert!(cache.get(ids[0]).is_some());
    assert!(
        cache.get(ids[1]).is_none(),
        "the least recently drawn went first"
    );
    assert!(
        cache.wants(ids[1], Instant::now()),
        "and is fetched again when seen"
    );

    // A failure is retried, but not at once.
    let failed = ChatAttachmentId::new();
    let now = Instant::now();
    cache.failed(failed, now);
    assert!(!cache.wants(failed, now));
    assert!(cache.wants(failed, now + THUMB_RETRY));
}

#[test]
fn a_chat_switched_away_from_an_agent_drops_its_drafts() {
    let (mut surface, chat, _commands) = surface_with_agent_chat();
    surface.add_draft(chat, Draft::pasted(png(2, 2), std::time::SystemTime::now()));
    let mut moved = surface.transcript.as_ref().unwrap().chat.clone();
    moved.current_model.provider = AiProviderId::parse(AiProviderId::OPENAI).unwrap();
    surface.apply_update(Update::ModelUpdated(moved, None));
    assert!(!surface.has_strip());
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(
            surface.lang,
            Key::AiAttachmentsRemovedForProvider
        ))
    );
}

#[test]
fn a_sent_messages_pictures_are_laid_out_under_its_words() {
    let (mut surface, chat, commands) = surface_with_agent_chat();
    let message = koru_ai_core::Message {
        id: koru_ai_core::MessageId::new(),
        chat_id: chat,
        ordinal: 0,
        role: koru_ai_core::ChatRole::User,
        state: koru_ai_core::MessageState::Complete,
        markdown: String::new(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(1),
        completed_at: Some(Timestamp::from_millis(1)),
    };
    let attachment = ChatAttachment {
        id: ChatAttachmentId::new(),
        message_id: Some(message.id),
        ordinal: Some(0),
        kind: AttachmentKind::Image,
        source: AttachmentSource::Clipboard,
        file_name: "shot.png".into(),
        media_type: "image/png".into(),
        byte_count: 10,
        dimensions: Some((300, 150)),
        origin: None,
        created_at: Timestamp::from_millis(1),
    };
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages.push(message);
    transcript.attachments.push(attachment.clone());
    surface.messages_dirty = true;
    surface.rebuild_messages(600.0);
    let layout = &surface.message_layouts[0];
    assert_eq!(layout.attachments, vec![Tile::of(&attachment)]);
    // A message of the picture alone is the picture's height and width.
    assert_eq!(message_content_height(layout), CARD_TILE_H);
    assert_eq!(message_content_width(layout), CARD_TILE_H * 2.0);

    // Drawn on screen, its thumbnail is asked for once — on the thumbnail
    // thread, never the reader's queue — and not again on the next draw.
    surface.request_thumbnails(&[(Rect::new(0.0, 0.0, 1.0, 1.0), Tile::of(&attachment))]);
    surface.request_thumbnails(&[(Rect::new(0.0, 0.0, 1.0, 1.0), Tile::of(&attachment))]);
    assert_eq!(surface.attachment_work.fetched, vec![attachment.id]);
    assert!(commands.try_iter().next().is_none());
    assert!(surface.attachment_deadline().is_some());
}

#[test]
fn a_thumbnail_decodes_to_a_bounded_brush() {
    let big = png(1_024, 256);
    let image = decode_thumbnail(&big.bytes).unwrap();
    assert_eq!((image.width, image.height), (512, 128));
    assert!(decode_thumbnail(b"not a picture").is_none());
}
