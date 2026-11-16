// SPDX-License-Identifier: MPL-2.0

//! **The field names are the contract**, so they are pinned here.
//!
//! A Rust rename breaks the build on both sides of the boundary, which is what
//! this crate exists for. What a compiler cannot catch is a rename that keeps
//! the *Rust* name and changes the *JSON* one — a stray `#[serde(rename)]`, a
//! `rename_all` added to a struct — because both sides move together and no
//! call site changes. That would silently break every client already shipped.
//!
//! These tests assert the JSON, not the struct.

use super::*;
use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};

fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn a_new_profile_carries_every_name_the_store_reads() {
    let body = serde_json::to_value(NewProfile {
        name: "Owner".into(),
        locale: Some("ko-KR".into()),
        purpose: Some("work".into()),
        avatar: Some("color:purple".into()),
        icon: Some("💼".into()),
        color: Some("blue".into()),
    })
    .unwrap();
    assert_eq!(
        keys(&body),
        ["avatar", "color", "icon", "locale", "name", "purpose"]
    );
}

/// **Absent is not English**, and the wire has to be able to say so.
#[test]
fn a_profile_with_no_locale_round_trips_as_absent_rather_than_a_guess() {
    let json = r#"{ "name": "Owner" }"#;
    let body: NewProfile = serde_json::from_str(json).unwrap();
    assert_eq!(body.locale, None);
    assert_eq!(body.purpose, None);
    assert_eq!(body.avatar, None);
    assert_eq!(body.icon, None);
    assert_eq!(body.color, None);
}

/// A required field is required. `name` carries no `#[serde(default)]`, so its
/// absence is an error rather than an empty string.
#[test]
fn a_profile_with_no_name_is_refused() {
    assert!(serde_json::from_str::<NewProfile>(r#"{ "locale": "en" }"#).is_err());
}

/// **An id is parsed, not validated.** Nothing downstream has to remember to
/// check one, because a malformed id cannot become a value.
#[test]
fn a_malformed_id_never_becomes_a_body() {
    assert!(
        serde_json::from_str::<NewWorkspace>(r#"{ "owner": "nonsense", "name": "W" }"#).is_err()
    );
    let good = format!(r#"{{ "owner": "{}", "name": "W" }}"#, ProfileId::new());
    assert!(serde_json::from_str::<NewWorkspace>(&good).is_ok());
}

/// And an id crosses as a bare string, not as a wrapper object — which is what
/// every other id on this boundary does, and what a client already parses.
#[test]
fn an_id_is_a_string_on_the_wire() {
    let id = DocumentId::new();
    let body = serde_json::to_value(Created { id }).unwrap();
    assert_eq!(body["id"], serde_json::Value::String(id.to_storage()));
}

#[test]
fn the_appearance_body_sends_all_four_every_time() {
    let body = serde_json::to_value(WorkspaceAppearance::default()).unwrap();
    assert_eq!(keys(&body), ["archived", "color", "icon", "pinned"]);
}

/// `color`, never `colour`. It is a name a *client* sends, so the British
/// spelling elsewhere in the tree must not leak onto the wire.
#[test]
fn the_appearance_body_spells_color_the_american_way() {
    let body = serde_json::to_value(WorkspaceAppearance {
        color: Some("purple".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(body["color"], "purple");
    assert!(body.get("colour").is_none());
}

#[test]
fn status_names_what_a_client_negotiates_on() {
    let body = serde_json::to_value(Status {
        api_version: 1,
        capabilities: vec!["loro-protocol".into()],
        channel: "stable".into(),
        started_at: 1_770_000_000_000,
    })
    .unwrap();
    assert_eq!(
        keys(&body),
        ["api_version", "capabilities", "channel", "started_at"]
    );
}

#[test]
fn imported_document_creation_carries_an_optional_trash_time() {
    let at = koru_store_core::model::Timestamp::from_millis(1_770_000_000_123);
    let body = serde_json::to_value(NewDocument {
        id: Some(DocumentId::new()),
        title: "Deleted note".into(),
        kind: "page".into(),
        mime: None,
        parent: None,
        index: None,
        trashed_at: Some(at),
        snapshot: Some(vec![1]),
    })
    .unwrap();
    assert_eq!(body["trashed_at"], 1_770_000_000_123_i64);
}

#[test]
fn a_failure_is_a_code_and_a_message() {
    let body = serde_json::to_value(Failure {
        code: "workspace_not_found".into(),
        message: "no row for the given id".into(),
        // Absent, not empty, on a failure that is not a parked hydration.
        waiting_on: Vec::new(),
        // And absent on one that is not a blob transfer hold.
        hold: None,
    })
    .unwrap();
    assert_eq!(keys(&body), ["code", "message"]);

    let waiting = serde_json::to_value(Failure {
        code: "blob_waiting_for_peer".into(),
        message: "the holder is offline".into(),
        waiting_on: vec!["Pixel 8".into()],
        hold: None,
    })
    .unwrap();
    assert_eq!(keys(&waiting), ["code", "message", "waiting_on"]);

    // **The hold rides beside the code** (D259), because the blob engine's
    // vocabulary is not the Store's and a client that recovered it by parsing
    // `message` would be branching on developer text. Both can be true at
    // once — a peer is offline *and* this device is not using Cloud for the
    // Workspace — and only the second is something a person can act on.
    let held = serde_json::to_value(Failure {
        code: "blob_waiting_for_peer".into(),
        message: "the holder is offline".into(),
        waiting_on: vec!["Pixel 8".into()],
        hold: Some("mode_refuses_transport".into()),
    })
    .unwrap();
    assert_eq!(keys(&held), ["code", "hold", "message", "waiting_on"]);
    assert_eq!(held["hold"], "mode_refuses_transport");
}

/// **Unknown fields are ignored, and that is deliberate.** It is what lets a
/// newer store add a counter without breaking an older client, and it is the
/// reason the internal reports can be typed at all. Pinned because
/// `deny_unknown_fields` sounds stricter and would make every added field a
/// breaking change.
#[test]
fn a_field_a_client_has_not_heard_of_is_ignored_rather_than_fatal() {
    let json = r#"{ "api_version": 1, "capabilities": [], "channel": "stable",
                    "started_at": 0, "something_new": 42 }"#;
    let status: Status = serde_json::from_str(json).unwrap();
    assert_eq!(status.api_version, 1);
}

// ------------------------------------------------------- personal attributes

/// The typed values are tagged, and the tags are the contract.
#[test]
fn a_personal_attribute_value_is_tagged_with_its_type() {
    let date = serde_json::to_value(PersonalAttributeValue::Date {
        date: "2026-08-30".into(),
    })
    .unwrap();
    assert_eq!(
        date,
        serde_json::json!({ "type": "date", "date": "2026-08-30" })
    );

    let zoned = serde_json::to_value(PersonalAttributeValue::DatetimeZoned {
        ms: 1_756_500_000_000,
        tz: "America/New_York".into(),
    })
    .unwrap();
    assert_eq!(
        zoned,
        serde_json::json!({ "type": "datetime_zoned", "ms": 1_756_500_000_000i64,
                            "tz": "America/New_York" })
    );

    let floating = serde_json::to_value(PersonalAttributeValue::DatetimeFloating {
        wall: "2026-08-30T09:00:00".into(),
    })
    .unwrap();
    assert_eq!(
        floating,
        serde_json::json!({ "type": "datetime_floating", "wall": "2026-08-30T09:00:00" })
    );
}

/// **The three temporal types are three tags on the wire**, and none of them
/// is an instant with a nullable zone ([ADR-183]).
///
/// A floating value carries no `tz` key because it has no zone *field* to
/// leave empty, and a zoned one always carries one because the zone is the
/// other half of the value. The retired `datetime` tag — the one that meant
/// either — does not deserialize at all, which is what stops a client from
/// sending the ambiguous shape.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[test]
fn the_wire_has_three_temporal_tags_and_no_nullable_zone() {
    let floating = serde_json::to_value(PersonalAttributeValue::DatetimeFloating {
        wall: "2026-08-30T09:00:00".into(),
    })
    .unwrap();
    assert_eq!(keys(&floating), ["type", "wall"]);

    let zoned = serde_json::to_value(PersonalAttributeValue::DatetimeZoned {
        ms: 42,
        tz: "UTC".into(),
    })
    .unwrap();
    assert_eq!(keys(&zoned), ["ms", "type", "tz"]);

    assert!(
        serde_json::from_str::<PersonalAttributeValue>(r#"{"type":"datetime","ms":42}"#).is_err(),
        "the ambiguous R0 tag is not a shape a client can send"
    );
    assert!(
        serde_json::from_str::<PersonalAttributeValue>(r#"{"type":"datetime_zoned","ms":42}"#)
            .is_err(),
        "a zoned value without its zone is not a value"
    );
}

/// `value: null` and an absent `value` both clear — the write body has no way
/// to store a null, because storage has no null to store.
#[test]
fn a_null_or_absent_value_reads_as_a_clear() {
    let explicit: SetPersonalAttribute =
        serde_json::from_str(r#"{ "entity": "e", "key": "koru.plan", "value": null }"#).unwrap();
    assert_eq!(explicit.value, None);
    let absent: SetPersonalAttribute =
        serde_json::from_str(r#"{ "entity": "e", "key": "koru.plan" }"#).unwrap();
    assert_eq!(absent.value, None);
}

#[test]
fn the_write_body_requires_the_entity_and_the_key() {
    assert!(serde_json::from_str::<SetPersonalAttribute>(r#"{ "key": "koru.plan" }"#).is_err());
    assert!(serde_json::from_str::<SetPersonalAttribute>(r#"{ "entity": "e" }"#).is_err());
}

#[test]
fn the_internal_reports_round_trip() {
    let report = internal::WorkspaceReport {
        workspace: WorkspaceId::new(),
        database_bytes: 4096,
        documents: 3,
        blobs: 1,
        blob_bytes: 12,
        lineage: Vec::new(),
        manifest_document: DocumentId::new(),
        deleted_at_millis: None,
        manifest_deletion: Some("active".into()),
        icon: None,
        color: None,
        blob_transfer: Some(internal::BlobTransferReport {
            referenced: 2,
            present: 1,
            durable: 1,
            uploading: 0,
            downloading: 1,
            missing: 1,
            bytes_complete: 12,
            bytes_total: 24,
            pending_reason: Some("storage_unavailable".into()),
            last_failure_code: None,
            direction: Some("download".into()),
            path: Some("cloud".into()),
            object_host: Some("us.data.koru.do".into()),
            oldest_retry_at_millis: Some(1_789),
            unreadable_references: 0,
            walks: 7,
            passes: 9,
            passes_skipped: 2,
            paused: true,
            preflight: Some(internal::BlobPreflightReport {
                files: 2,
                upload_files: 1,
                upload_bytes: 12,
                download_files: 1,
                unreadable: 0,
                at_millis: 1_700,
            }),
            coverage: Some(internal::BlobCoverageReport {
                state: "incomplete".into(),
                current: false,
                documents_required: 3,
                documents_missing: 0,
                blobs_referenced: 2,
                blobs_durable: 1,
                blobs_uploading: 0,
                blobs_missing: 1,
                unreadable_references: 0,
                sample: "0011223344556677".into(),
            }),
        }),
    };
    let json = serde_json::to_string(&report).unwrap();
    let back: internal::WorkspaceReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.workspace, report.workspace);
    assert_eq!(back.database_bytes, 4096);
    let transfer = back.blob_transfer.expect("the blob transfer report");
    assert_eq!(transfer.referenced, 2);
    assert_eq!(transfer.path.as_deref(), Some("cloud"));
    // **Two numbers, not one.** A missing picture must never read as a missing
    // document on the surface a person or a support thread looks at.
    let coverage = transfer.coverage.expect("the coverage report");
    assert_eq!(coverage.documents_missing, 0);
    assert_eq!(coverage.blobs_missing, 1);
    assert!(!coverage.current);
    // **Ids, counts and codes only.** A report that had learned a filename,
    // a title or a capability would say so here.
    for forbidden in ["filename", "title", "capability", "token", "secret"] {
        assert!(
            !json.contains(forbidden),
            "a workspace report must not carry {forbidden}"
        );
    }
}

/// **Which documents wait, for whom** — ids, titles, counts and device
/// names, and nothing a person would read as an address.
#[test]
fn the_waiting_documents_carry_titles_counts_and_names_only() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let answer = CloudWaitingDocuments {
        workspace: Some(workspace),
        documents: vec![CloudWaitingDocument {
            document,
            title: "Trip notes".into(),
            missing_files: 6,
            holders: vec!["Pixel 8".into()],
        }],
    };
    let json = serde_json::to_value(&answer).unwrap();
    assert_eq!(keys(&json), ["documents", "workspace"]);
    assert_eq!(
        keys(&json["documents"][0]),
        ["document", "holders", "missing_files", "title"]
    );
    assert_eq!(json["documents"][0]["missing_files"], 6);
    let text = json.to_string();
    for forbidden in ["b3:", "hash", "filename", "capability"] {
        assert!(!text.contains(forbidden), "it must not carry {forbidden}");
    }
    let back: CloudWaitingDocuments = serde_json::from_value(json).unwrap();
    assert_eq!(back, answer);
    // An older Store that knew no holders still reads as *none known*.
    let bare: CloudWaitingDocument = serde_json::from_str(&format!(
        r#"{{"document":"{}","title":"","missing_files":1}}"#,
        document.to_storage()
    ))
    .unwrap();
    assert!(bare.holders.is_empty());
}

/// **One document's own Cloud state** — a yes/no and four counts, and nothing
/// a person would read as an address, a name or a type.
#[test]
fn a_documents_coverage_carries_counts_only() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let answer = internal::CloudDocumentCoverage {
        workspace: Some(workspace),
        document: Some(document),
        synced: Some(true),
        files: Some(internal::CloudDocumentFiles {
            images_total: 7,
            images_synced: 7,
            files_total: 1,
            files_synced: 0,
        }),
    };
    let json = serde_json::to_value(&answer).unwrap();
    assert_eq!(keys(&json), ["document", "files", "synced", "workspace"]);
    assert_eq!(
        keys(&json["files"]),
        [
            "files_synced",
            "files_total",
            "images_synced",
            "images_total"
        ]
    );
    let text = json.to_string();
    for forbidden in ["b3:", "hash", "filename", "mime", "image/"] {
        assert!(!text.contains(forbidden), "it must not carry {forbidden}");
    }
    let back: internal::CloudDocumentCoverage = serde_json::from_value(json).unwrap();
    assert_eq!(back, answer);
    // Nothing measured and nothing known reads as absent, never as zero.
    let bare: internal::CloudDocumentCoverage = serde_json::from_str("{}").unwrap();
    assert_eq!(bare.synced, None);
    assert_eq!(bare.files, None);
}

// ------------------------------------------------------------------ search

/// A search answer exactly as a store built before reading sends it: no
/// `page` on any hit, no `reading` on the page.
const SEARCH_ANSWER_BEFORE_READING: &str = r#"{
  "page": {
    "hits": [{
      "workspace": "ws", "workspace_name": "Acme", "document": "doc",
      "node": "node", "document_kind": "file", "mime": "application/pdf",
      "kind": "file", "field": "title", "title": "Manual.pdf",
      "breadcrumb": "Acme", "breadcrumb_spans": [], "snippet": "Manual.pdf",
      "spans": [{ "start": 0, "end": 6 }], "lifecycle": "live",
      "trashed_at": null, "trashed_by": null, "score": 12.5,
      "observed_modified_at_ms": null, "workspace_last_used_at_ms": null
    }],
    "next_offset": null,
    "updating_workspaces": [],
    "request_seq": 4,
    "corrected_query": null
  }
}"#;

/// **An old search answer still parses.** The page on a hit and the reading
/// counts on a page are additive: a newer client reading an older store gets
/// `None` for both, which a surface draws as no prefix and no line.
#[test]
fn a_search_answer_from_before_reading_still_parses() {
    let answer: search::SearchResponse =
        serde_json::from_str(SEARCH_ANSWER_BEFORE_READING).unwrap();
    assert_eq!(answer.page.hits.len(), 1);
    assert_eq!(answer.page.hits[0].page, None);
    assert_eq!(answer.page.reading, None);
    assert_eq!(answer.page.request_seq, 4);
}

/// **And a store with nothing to say about reading sends the old bytes.** Both
/// new fields are left out when absent, so an answer with no reading in it is
/// the answer an older client already parses, key for key.
#[test]
fn an_answer_with_no_reading_in_it_is_the_old_shape() {
    let answer: search::SearchResponse =
        serde_json::from_str(SEARCH_ANSWER_BEFORE_READING).unwrap();
    let sent = serde_json::to_value(&answer).unwrap();
    let before: serde_json::Value = serde_json::from_str(SEARCH_ANSWER_BEFORE_READING).unwrap();
    assert_eq!(sent, before);
}

/// The new names are the contract (TECHNICAL §10): `field::READ` is `read`,
/// a hit carries `page`, and a page carries `reading` with its five counts.
#[test]
fn the_reading_fields_are_named_as_the_design_says() {
    assert_eq!(search::field::READ, "read");

    let mut answer: search::SearchResponse =
        serde_json::from_str(SEARCH_ANSWER_BEFORE_READING).unwrap();
    answer.page.hits[0].field = search::field::READ.into();
    answer.page.hits[0].page = Some(3);
    answer.page.reading = Some(search::ReadingCounts {
        unread: 1_240,
        total: 8_300,
        needs_reader: 900,
        not_here: 12,
        offer: true,
    });
    let sent = serde_json::to_value(&answer).unwrap();
    assert_eq!(sent["page"]["hits"][0]["page"], 3);
    assert_eq!(sent["page"]["hits"][0]["field"], "read");
    assert_eq!(
        keys(&sent["page"]["reading"]),
        ["needs_reader", "not_here", "offer", "total", "unread"]
    );
    let back: search::SearchResponse = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!(back, answer);

    // An older client, which knows neither field, reads the newer answer: it
    // is the old shape plus two keys it ignores.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OlderPage {
        hits: Vec<OlderHit>,
        next_offset: Option<usize>,
        updating_workspaces: Vec<String>,
        request_seq: u64,
        corrected_query: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OlderHit {
        workspace: String,
        field: String,
        snippet: String,
        score: f64,
    }
    let older: OlderPage = serde_json::from_value(sent["page"].clone()).unwrap();
    assert_eq!(older.hits[0].field, "read");
}
