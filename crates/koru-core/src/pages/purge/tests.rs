// SPDX-License-Identifier: MPL-2.0

use super::*;

fn vv(peer: u64, counter: i32) -> VersionVector {
    let mut version = VersionVector::new();
    version.insert(peer, counter);
    version
}

#[test]
fn coverage_joins_concurrent_deletes_in_either_order() {
    let doc = Uuid::new_v4();
    let a = Event::new(doc, doc, Some(&vv(1, 3)), "a");
    let b = Event::new(doc, doc, Some(&vv(2, 4)), "b");
    let forward = Coverage::of([&a, &b]).unwrap();
    assert_eq!(forward, Coverage::of([&b, &a, &a]).unwrap());
    assert!(forward.covers(&vv(1, 3)));
    assert!(forward.covers(&vv(2, 4)));
    assert!(!forward.covers(&vv(1, 4)));
}

#[test]
fn missing_evidence_is_not_known_empty_coverage() {
    let doc = Uuid::new_v4();
    let known = Event::new(doc, doc, Some(&VersionVector::new()), "a");
    let unknown = Event::new(doc, doc, None, "b");
    assert_eq!(Coverage::of([&known, &unknown]), Some(Coverage::Unknown));
    assert_eq!(Coverage::of([&unknown, &known]), Some(Coverage::Unknown));
    assert!(!Coverage::Unknown.covers(&VersionVector::new()));
    assert_eq!(Coverage::of([]), None);
}

#[test]
fn actor_is_bounded_without_cutting_unicode() {
    let doc = Uuid::new_v4();
    let event = Event::new(doc, doc, None, &"日".repeat(60));
    assert_eq!(event.by.len(), 126);
    assert_eq!(event.validate(), Ok(()));
}

#[test]
fn malformed_id_version_or_actor_is_refused() {
    let doc = Uuid::new_v4();
    let mut event = Event::new(doc, doc, None, "actor");
    event.event_id = Uuid::nil();
    assert_eq!(event.validate(), Err(EventError::Identity));
    event.event_id = Uuid::new_v4();
    event.version = Some(vec![255, 255]);
    assert_eq!(event.validate(), Err(EventError::Version));
    event.version = None;
    event.by = "a".repeat(MAX_ACTOR_BYTES + 1);
    assert_eq!(event.validate(), Err(EventError::ActorTooLong));
}

#[test]
fn wire_round_trip_preserves_the_exact_payload_and_rejects_unknown_fields() {
    let doc = Uuid::new_v4();
    let event = Event::new(doc, doc, Some(&vv(42, 5)), "Élodie");
    let encoded = serde_json::to_vec(&event).unwrap();
    assert_eq!(serde_json::from_slice::<Event>(&encoded).unwrap(), event);
    let mut value = serde_json::to_value(&event).unwrap();
    value["timestamp"] = 1.into();
    assert!(serde_json::from_value::<Event>(value).is_err());
}
