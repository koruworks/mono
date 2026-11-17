// SPDX-License-Identifier: MPL-2.0

//! `m8_wire_limits`: the frame limit, and a page with one 786,432-byte
//! record.

use super::{id, negotiated, owner, vector};
use crate::{
    Changes, Entry, FRAME_HEADER_BYTES, FrameDecoder, Limits, MAX_FRAME_BYTES, MAX_RECORD_BYTES,
    MessageRecord, PageBuilder, Record, Refused, Role, Violation, WireError, WireMessage,
    decode_body, encode, encode_body, limits,
};

fn refused_of(body_bytes: usize) -> WireMessage {
    let empty = encode_body(&WireMessage::Refused(Refused::new("")))
        .unwrap()
        .len();
    WireMessage::Refused(Refused::new(&"r".repeat(body_bytes - empty)))
}

fn message_record() -> MessageRecord {
    let Record::Message(record) = serde_json::from_str(vector!("record_message.json")).unwrap()
    else {
        panic!("a message record");
    };
    *record
}

/// The message record whose JSON is exactly `bytes` long.
fn record_of(bytes: usize) -> MessageRecord {
    let mut record = message_record();
    record.message.markdown.clear();
    let base = record.encoded_len();
    record.message.markdown = "m".repeat(bytes - base);
    assert_eq!(record.encoded_len(), bytes);
    record
}

fn page(entries: Vec<Entry>) -> Changes {
    let to_cursor = entries.last().map_or(0, |entry| entry.seq);
    Changes {
        owner_device: owner(),
        feed_id: "0123456789abcdef0123456789abcdef".to_owned(),
        boot_id: "fedcba9876543210fedcba9876543210".to_owned(),
        from_cursor: 0,
        to_cursor,
        reset: false,
        caught_up: false,
        head: to_cursor,
        live: None,
        withheld_bots: None,
        entries,
    }
}

fn small_entry(seq: u64) -> Entry {
    Entry::removed(
        seq,
        "message",
        id::<uuid::Uuid>("08").to_string(),
        None,
        None,
    )
}

#[test]
fn m8_wire_limits_the_numbers_are_the_contracts() {
    assert_eq!(MAX_FRAME_BYTES, 1_048_576);
    assert_eq!(MAX_RECORD_BYTES, 786_432);
    assert_eq!(limits::MAX_PAGE_ENTRIES, 64);
    assert_eq!(limits::MAX_PAGE_BYTES, 524_288);
    assert_eq!(limits::FEED_WINDOW, 2);
    assert_eq!(limits::LIVE_BUFFER_EVENTS, 64);
    assert_eq!(limits::MAX_INFLIGHT_COMMANDS, 4);
    assert_eq!(limits::MAX_WATCHES_PER_STREAM, 4);
    assert_eq!(limits::MAX_STREAMS_PER_CONNECTION, 4);
    assert_eq!(limits::MAX_PEER_DEVICES, 8);
    assert_eq!(limits::HELLO_TIMEOUT_MS, 10_000);
    assert_eq!(limits::COMMAND_IDLE_TIMEOUT_MS, 120_000);
    assert_eq!(limits::ACK_TIMEOUT_MS, 60_000);
    assert_eq!(limits::KEEPALIVE_INTERVAL_MS, 30_000);
    assert_eq!(limits::FEED_SILENCE_TIMEOUT_MS, 90_000);
    assert_eq!(limits::MAX_CLIENT_BUILD_BYTES, 64);
    assert_eq!(
        Limits::this_build(),
        Limits {
            max_frame_bytes: 1_048_576,
            max_page_entries: 64,
            max_page_bytes: 524_288,
            feed_window: 2,
            live_buffer_events: 64,
            max_inflight_commands: 4,
        }
    );
    assert_eq!(crate::PROTOCOL_TOKEN, "koru/bots/1");
    assert_eq!((crate::MIN_VERSION, crate::MAX_VERSION), (1, 1));
    assert_eq!(crate::REPLICA_FORMAT, 1);
}

#[test]
fn m8_wire_limits_a_frame_of_the_limit_is_written_and_one_byte_more_is_not() {
    let largest = refused_of(MAX_FRAME_BYTES);
    let framed = encode(&largest).unwrap();
    assert_eq!(framed.len(), FRAME_HEADER_BYTES + MAX_FRAME_BYTES);
    assert_eq!(&framed[..4], &(MAX_FRAME_BYTES as u32).to_be_bytes());
    assert_eq!(decode_body(&framed[4..]).unwrap(), largest);

    let over = refused_of(MAX_FRAME_BYTES + 1);
    assert_eq!(
        encode(&over),
        Err(WireError::FrameTooLarge {
            bytes: MAX_FRAME_BYTES + 1
        })
    );
    let body = serde_json::to_vec(&over).unwrap();
    assert_eq!(
        decode_body(&body),
        Err(WireError::FrameTooLarge {
            bytes: MAX_FRAME_BYTES + 1
        })
    );
}

#[test]
fn m8_wire_limits_the_reader_stops_at_the_length_of_an_oversized_frame() {
    // The largest frame arrives whole.
    let largest = refused_of(MAX_FRAME_BYTES);
    let mut decoder = FrameDecoder::new();
    decoder.push(&encode(&largest).unwrap()).unwrap();
    assert_eq!(decoder.next().unwrap(), Some(largest));

    // One byte more is refused at its four length bytes: nothing of the
    // body is waited for or buffered, and the stream is dead afterwards.
    let mut decoder = FrameDecoder::new();
    let announced = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
    decoder.push(&announced[..3]).unwrap();
    assert_eq!(
        decoder.push(&announced[3..]),
        Err(WireError::FrameTooLarge {
            bytes: MAX_FRAME_BYTES + 1
        })
    );
    assert_eq!(decoder.buffered(), 4);
    assert_eq!(decoder.next(), Err(WireError::Poisoned));
    assert_eq!(decoder.push(b"{}"), Err(WireError::Poisoned));

    // A length of zero is no frame.
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.push(&[0, 0, 0, 0]), Err(WireError::EmptyFrame));
    assert_eq!(decoder.next(), Err(WireError::Poisoned));

    // A malformed body poisons the stream as well.
    let mut decoder = FrameDecoder::new();
    decoder.push(&[0, 0, 0, 2, b'{', b'}']).unwrap();
    assert_eq!(decoder.next(), Err(WireError::Malformed));
    assert_eq!(decoder.next(), Err(WireError::Poisoned));
}

#[test]
fn m8_wire_limits_frames_arrive_in_any_pieces() {
    let first = decode_body(vector!("ack.json").as_bytes()).unwrap();
    let second = decode_body(vector!("changes.json").as_bytes()).unwrap();
    let mut bytes = encode(&first).unwrap();
    bytes.extend(encode(&second).unwrap());

    // Byte by byte.
    let mut decoder = FrameDecoder::new();
    let mut read = Vec::new();
    for byte in &bytes {
        decoder.push(std::slice::from_ref(byte)).unwrap();
        while let Some(message) = decoder.next().unwrap() {
            read.push(message);
        }
    }
    assert_eq!(read, [first.clone(), second.clone()]);
    assert_eq!(decoder.buffered(), 0);

    // Both in one delivery, with the start of a third.
    let mut decoder = FrameDecoder::new();
    bytes.extend([0, 0]);
    decoder.push(&bytes).unwrap();
    assert_eq!(decoder.next().unwrap(), Some(first));
    assert_eq!(decoder.next().unwrap(), Some(second));
    assert_eq!(decoder.next().unwrap(), None);
    assert_eq!(decoder.buffered(), 2);
}

#[test]
fn m8_wire_limits_a_page_with_one_record_of_the_largest_size() {
    let record = record_of(MAX_RECORD_BYTES);
    // At the limit the record is sent whole.
    let fitted = record.clone().fitted();
    assert_eq!(fitted, record);
    assert!(!fitted.body_omitted);

    let entry = Entry::present(1, id::<uuid::Uuid>("08").to_string(), fitted.into());
    assert!(entry.encoded_len() > limits::MAX_PAGE_BYTES as usize);

    // A page always holds at least one entry, so this one is taken though
    // it is over the page's byte limit; nothing may follow it.
    let mut builder = PageBuilder::new(Limits::this_build());
    builder.push(entry).unwrap();
    assert!(builder.push(small_entry(2)).is_err());
    assert_eq!(builder.len(), 1);
    let page = page(builder.finish());
    page.check_limits(&Limits::this_build()).unwrap();

    // The page exceeds `max_page_bytes` and still fits a frame.
    let message = WireMessage::Changes(page);
    let framed = encode(&message).unwrap();
    assert!(framed.len() > limits::MAX_PAGE_BYTES as usize);
    assert!(framed.len() <= FRAME_HEADER_BYTES + MAX_FRAME_BYTES);
    let mut decoder = FrameDecoder::new();
    decoder.push(&framed).unwrap();
    let read = decoder.next().unwrap().unwrap();
    assert_eq!(read, message);
    negotiated(Role::Feed).check_from_owner(&read).unwrap();
}

#[test]
fn m8_wire_limits_a_record_over_the_limit_is_sent_without_its_body() {
    let record = record_of(MAX_RECORD_BYTES + 1);
    let fitted = record.clone().fitted();
    assert!(fitted.body_omitted);
    assert!(fitted.message.markdown.is_empty());
    assert!(fitted.blocks.is_empty());
    // What was beside the text stays while the record fits without it.
    assert_eq!(fitted.tool_activities, record.tool_activities);
    assert_eq!(fitted.notices, record.notices);
    assert_eq!(fitted.usage, record.usage);
    assert_eq!(fitted.attachment_count, record.attachment_count);
    assert_eq!(fitted.message.id, record.message.id);
    assert!(fitted.encoded_len() < 4_096);

    // Still too large with the text gone: the rest of the body goes too.
    let mut heavy = message_record();
    let mut activity = heavy.tool_activities[0].clone();
    activity.agent_output = Some("o".repeat(MAX_RECORD_BYTES));
    heavy.tool_activities.push(activity);
    let fitted = heavy.clone().fitted();
    assert!(fitted.body_omitted);
    assert!(fitted.tool_activities.is_empty());
    assert!(fitted.notices.is_empty() && fitted.feedback.is_empty());
    assert_eq!(fitted.sources, None);
    assert_eq!(fitted.usage, heavy.usage);
    assert!(fitted.encoded_len() < 4_096);
}

#[test]
fn m8_wire_limits_a_page_holds_sixty_four_entries_and_half_a_mebibyte() {
    // The entry count.
    let mut builder = PageBuilder::new(Limits::this_build());
    for seq in 1..=64 {
        builder.push(small_entry(seq)).unwrap();
    }
    assert_eq!(builder.push(small_entry(65)).unwrap_err().seq, 65);
    let full = page(builder.finish());
    assert_eq!(full.entries.len(), 64);
    full.check_limits(&Limits::this_build()).unwrap();
    let mut over = full.clone();
    over.entries.push(small_entry(65));
    assert_eq!(
        over.check_limits(&Limits::this_build()),
        Err(Violation::PageTooLarge)
    );

    // The byte budget: records of 200,000 bytes, two fit and a third does
    // not.
    let record = |seq: u64| {
        Entry::present(
            seq,
            id::<uuid::Uuid>("08").to_string(),
            record_of(200_000).into(),
        )
    };
    let mut builder = PageBuilder::new(Limits::this_build());
    builder.push(record(1)).unwrap();
    builder.push(record(2)).unwrap();
    assert!(builder.bytes() <= limits::MAX_PAGE_BYTES as usize);
    assert_eq!(builder.push(record(3)).unwrap_err().seq, 3);
    let two = page(builder.finish());
    two.check_limits(&Limits::this_build()).unwrap();
    let mut three = two.clone();
    three.entries.push(record(3));
    assert_eq!(
        three.check_limits(&Limits::this_build()),
        Err(Violation::PageTooLarge)
    );
}

#[test]
fn m8_wire_limits_a_page_that_is_not_the_next_one_fails_its_checks() {
    let WireMessage::Changes(good) = decode_body(vector!("changes.json").as_bytes()).unwrap()
    else {
        panic!("a changes page");
    };
    let owner = owner();
    let expected = crate::PageExpectation {
        owner_device: &owner,
        feed_id: "0123456789abcdef0123456789abcdef",
        boot_id: "fedcba9876543210fedcba9876543210",
        cursor: 37,
    };
    good.check_page(&expected).unwrap();

    // Another cursor, another feed, another start.
    let mut stale = expected.clone();
    stale.cursor = 36;
    assert_eq!(good.check_page(&stale), Err(Violation::PageOutOfOrder));
    let mut other = good.clone();
    other.feed_id = "f".repeat(32);
    assert_eq!(other.check_page(&expected), Err(Violation::PageOutOfOrder));
    let mut other = good.clone();
    other.boot_id = "0".repeat(32);
    assert_eq!(other.check_page(&expected), Err(Violation::PageOutOfOrder));

    // `seq` out of order, repeated, or outside `(from_cursor, to_cursor]`.
    let mut swapped = good.clone();
    swapped.entries.swap(0, 1);
    assert_eq!(
        swapped.check_page(&expected),
        Err(Violation::PageOutOfOrder)
    );
    let mut repeated = good.clone();
    repeated.entries[1].seq = 38;
    assert_eq!(
        repeated.check_page(&expected),
        Err(Violation::PageOutOfOrder)
    );
    let mut early = good.clone();
    early.entries[0].seq = 37;
    assert_eq!(early.check_page(&expected), Err(Violation::PageOutOfOrder));
    let mut late = good.clone();
    late.entries[4].seq = 43;
    assert_eq!(late.check_page(&expected), Err(Violation::PageOutOfOrder));

    // Counts belong to a caught-up page and only to it.
    let mut no_counts = good.clone();
    no_counts.live = None;
    assert_eq!(
        no_counts.check_page(&expected),
        Err(Violation::RecordMismatch)
    );
    let mut early_counts = good.clone();
    early_counts.caught_up = false;
    assert_eq!(
        early_counts.check_page(&expected),
        Err(Violation::RecordMismatch)
    );
}
