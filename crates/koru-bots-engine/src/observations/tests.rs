// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn live_feed_bounds_memory_reports_gaps_and_checks_membership() {
    let (db, records) = crate::operations::tests::leased_fixture(1);
    let record = &records[0];
    let turn = record.lease.as_ref().unwrap().binding.turn;
    let observation = Observation {
        thread: record.key.thread,
        bot: record.key.bot,
        turn,
        event: AgentEvent::MessageDelta {
            turn_id: turn.id.to_string().parse().unwrap(),
            text: "synthetic".into(),
        },
    };
    let person = Person {
        owner: record.key.scope.clone(),
        sender: "person".into(),
    };
    let mut events = LiveEvents::default();
    for _ in 0..600 {
        events.push(observation.clone()).unwrap();
    }
    assert_eq!(events.entries.len(), 512);
    let page = events
        .read(&db, &person, record.key.thread, Some(events.boot), 1)
        .unwrap();
    assert!(page.gap);
    assert_eq!(page.entries.len(), crate::messages::PAGE_LIMIT);
    assert_eq!(page.cursor, 152);
    let mut cursor = page.cursor;
    let mut count = page.entries.len();
    while cursor < 600 {
        let next = events
            .read(&db, &person, record.key.thread, Some(page.boot), cursor)
            .unwrap();
        assert!(!next.gap);
        assert!(next.entries.len() <= crate::messages::PAGE_LIMIT);
        assert!(next.cursor > cursor);
        count += next.entries.len();
        cursor = next.cursor;
    }
    assert_eq!(count, 512);
    assert!(
        events
            .read(&db, &person, record.key.thread, Some(events.boot), 600)
            .unwrap()
            .entries
            .is_empty()
    );
    let stranger = Person {
        owner: person.owner.clone(),
        sender: "stranger".into(),
    };
    assert_eq!(
        events
            .read(&db, &stranger, record.key.thread, None, 0)
            .err(),
        Some(Error::NotAllowed)
    );
    let mut oversized = observation.clone();
    if let AgentEvent::MessageDelta { text, .. } = &mut oversized.event {
        *text = "x".repeat(koru_bot_client_wire::MAX_FRAME_BYTES);
    }
    assert_eq!(events.push(oversized).err(), Some(Error::InputTooSmall));
    assert_eq!(events.next, 601);
}
