// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn an_accepted_input_is_not_a_running_reply_and_publish_uses_the_reserved_identity() {
    let mut f = Fixture::new();
    let accepted = f.send("queued");
    assert_eq!(f.count("turns"), 0);
    let delivery = f.claim(Resume::Rebuilt);
    let reserved: String =
        f.db.connection
            .query_row(
                "SELECT reply_message FROM turns WHERE id=?1",
                [delivery.turn.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    let reserved = Uuid::parse_str(&reserved).unwrap();
    assert_ne!(reserved, accepted.message);
    assert_ne!(reserved, delivery.turn.id);
    assert_eq!(f.count("messages"), 1); // Identity reserved, no invented answer.
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let request = reply(&delivery);
    let published = f.db.publish_reply(&request, 9).unwrap();
    assert_eq!(published.answer.message, reserved);
    let replay = f.db.publish_reply(&request, 10).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.answer, published.answer);
    let messages = f.db.read_messages(&f.person, f.thread, 0).unwrap();
    assert_eq!(messages[0].created_at_ms, 1);
    assert_eq!(messages[1].created_at_ms, 9);
}

#[test]
fn recent_history_pages_are_stable_under_new_messages_and_keep_every_identity() {
    let mut f = Fixture::new();
    let mut expected = vec![];
    for index in 0..140 {
        expected.push(f.send(&index.to_string()).message);
    }
    let newest =
        f.db.read_recent_messages(&f.person, f.thread, None)
            .unwrap();
    assert_eq!(newest.len(), PAGE_LIMIT);
    let mut before = newest.first().map(|m| m.sequence);
    let mut actual: Vec<_> = newest.into_iter().map(|m| m.id).collect();
    let later = f.send("new arrival");
    loop {
        let page =
            f.db.read_recent_messages(&f.person, f.thread, before)
                .unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.windows(2).all(|p| p[0].sequence < p[1].sequence));
        before = page.first().map(|m| m.sequence);
        let mut prefix: Vec<_> = page.into_iter().map(|m| m.id).collect();
        prefix.append(&mut actual);
        actual = prefix;
    }
    assert_eq!(actual, expected);
    assert!(!actual.contains(&later.message));
    assert_eq!(
        f.db.read_recent_messages(&f.person, f.thread, Some(0))
            .err(),
        Some(Error::InvalidRequest)
    );
    let other = Person {
        owner: "other".into(),
        sender: f.person.sender.clone(),
    };
    assert_eq!(
        f.db.read_recent_messages(&other, f.thread, None).err(),
        Some(Error::NotAllowed)
    );
}
