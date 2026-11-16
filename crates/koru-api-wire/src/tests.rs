// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A federated answer as a gateway wrote it before reading existed.
const FEDERATED_BEFORE_READING: &str = r#"{
  "page": {
    "hits": [],
    "next_cursor": null,
    "updating_workspaces": [],
    "request_seq": 7,
    "corrected_query": null
  },
  "sources": [{ "source": "store", "state": "complete" }]
}"#;

/// **An old federated answer still parses** (OCR Plan 2.4): the reading
/// counts are additive, and a newer window reading an older gateway gets
/// `None`, which it draws as no line.
#[test]
fn a_federated_answer_from_before_reading_still_parses() {
    let answer: FederatedSearchResponse = serde_json::from_str(FEDERATED_BEFORE_READING).unwrap();
    assert_eq!(answer.page.reading, None);
    assert_eq!(answer.page.request_seq, 7);
    // And with nothing to say, the old bytes go out, key for key.
    let sent = serde_json::to_value(&answer).unwrap();
    let before: serde_json::Value = serde_json::from_str(FEDERATED_BEFORE_READING).unwrap();
    assert_eq!(sent, before);
}

/// The counts cross the gateway under the Store's own name and shape, and an
/// older window, which knows nothing of them, still reads the page.
#[test]
fn the_reading_counts_cross_the_gateway() {
    let mut answer: FederatedSearchResponse =
        serde_json::from_str(FEDERATED_BEFORE_READING).unwrap();
    answer.page.reading = Some(koru_store_wire::search::ReadingCounts {
        unread: 5,
        total: 5,
        needs_reader: 5,
        not_here: 0,
        offer: true,
    });
    let sent = serde_json::to_value(&answer).unwrap();
    assert_eq!(sent["page"]["reading"]["offer"], true);
    assert_eq!(sent["page"]["reading"]["needs_reader"], 5);
    let back: FederatedSearchResponse = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!(back, answer);

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OlderPage {
        hits: Vec<serde_json::Value>,
        next_cursor: Option<String>,
        updating_workspaces: Vec<String>,
        request_seq: u64,
        corrected_query: Option<String>,
    }
    let older: OlderPage = serde_json::from_value(sent["page"].clone()).unwrap();
    assert_eq!(older.request_seq, 7);
}
