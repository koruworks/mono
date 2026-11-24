// SPDX-License-Identifier: MPL-2.0

use super::*;
use loro::VersionVector;

fn event(peers: u64) -> Event {
    let mut version = VersionVector::new();
    for peer in 1..=peers {
        version.insert(peer, 1);
    }
    let doc = uuid::Uuid::new_v4();
    Event::new(doc, doc, Some(&version), "actor")
}

#[test]
fn both_carriers_fit_or_version_is_omitted_once() {
    let small = event(1);
    assert!(fits(&small));
    assert_eq!(bounded_event(small.clone()).unwrap(), small);
    let large = event(4000);
    assert!(!fits(&large));
    let bounded = bounded_event(large.clone()).unwrap();
    assert_eq!(bounded.event_id, large.event_id);
    assert!(bounded.version.is_none());
    assert!(fits(&bounded));
    assert_eq!(bounded_event(bounded.clone()).unwrap(), bounded);
}

#[test]
fn complete_page_is_bounded_at_the_transition() {
    let first_too_large = (1..2000).find(|n| !fits(&event(*n))).unwrap();
    let before = event(first_too_large - 1);
    assert!(bounded_event(before.clone()).unwrap().version.is_some());
    let at = event(first_too_large);
    assert!(bounded_event(at).unwrap().version.is_none());
    assert!(
        bounded_event(event(first_too_large + 1))
            .unwrap()
            .version
            .is_none()
    );
    let encoded = encode_record(&Record::Control(request(before))).unwrap();
    assert!(encoded.len() <= 4101);
}

#[test]
fn cursor_cannot_skip_last_event_or_reorder_a_page() {
    let e = event(1);
    let entry = Entry {
        sequence: 2,
        document_id: e.document_id,
        state: State::Deleting,
        event: e,
    };
    assert!(validate_page(std::slice::from_ref(&entry), 2).is_ok());
    assert!(validate_page(std::slice::from_ref(&entry), 3).is_err());
    assert!(validate_page(&[entry.clone(), entry], 2).is_err());
    assert!(validate_page(&[], i64::MAX as u64 + 1).is_err());
}

#[test]
fn exact_full_response_payload_limit_is_inclusive() {
    let encoded = |event: Event| {
        serde_json::to_vec(&page(
            vec![Entry {
                sequence: i64::MAX as u64,
                document_id: event.document_id,
                state: State::Deleting,
                event,
            }],
            i64::MAX as u64,
        ))
        .unwrap()
        .len()
    };
    let mut value = (1..2000)
        .map(event)
        .find(|event| (3980..4096).contains(&encoded(event.clone())))
        .unwrap();
    value.by.clear();
    let remaining = 4096 - encoded(value.clone());
    assert!(remaining < 128);
    value.by = "x".repeat(remaining);
    assert_eq!(encoded(value.clone()), 4096);
    assert!(fits(&value));
    value.by.pop();
    assert_eq!(encoded(value.clone()), 4095);
    assert!(fits(&value));
    value.by.push_str("xx");
    assert_eq!(encoded(value.clone()), 4097);
    assert!(!fits(&value));
    assert!(bounded_event(value).unwrap().version.is_none());
}

#[test]
fn exact_request_payload_limit_is_inclusive_even_when_response_needs_omission() {
    let size = |event: Event| serde_json::to_vec(&request(event)).unwrap().len();
    let mut value = (1..2000)
        .map(event)
        .find(|event| (3980..4096).contains(&size(event.clone())))
        .unwrap();
    value.by.clear();
    value.by = "x".repeat(4096 - size(value.clone()));
    assert_eq!(size(value.clone()), 4096);
    assert!(encode_record(&Record::Control(request(value.clone()))).is_ok());
    assert!(
        !fits(&value),
        "the longer full response decides what is persisted"
    );
    value.by.pop();
    assert!(encode_record(&Record::Control(request(value.clone()))).is_ok());
    value.by.push_str("xx");
    assert_eq!(size(value.clone()), 4097);
    assert!(encode_record(&Record::Control(request(value.clone()))).is_err());
    assert!(bounded_event(value).unwrap().version.is_none());
}
