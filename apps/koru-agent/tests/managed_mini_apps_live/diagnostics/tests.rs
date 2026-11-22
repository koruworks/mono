// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn synthetic_reply_diagnostic_scrubs_chunk_split_secrets_paths_urls_ids_and_stays_bounded() {
    let capability = mini_apps::Capability::mint().0;
    let mut reply = AssistantReply::default();
    for text in [
        "Cannot create because invalid_request. Password hun",
        "tertwo SECRET_",
        "SENTINEL https://example.invalid/private C:\\private\\data ../hidden alice@example.invalid ",
        "123e4567-e89b-42d3-a456-426614174000 sk-supersecret abcdefghijklmnopqrstuvwxyzABCDEF ",
        capability.as_str(),
    ] {
        reply.push(text);
    }
    let safe = reply.scrubbed();
    assert!(safe.contains("Cannot create because invalid_request"));
    for secret in [
        "huntertwo",
        "SECRET",
        "SENTINEL",
        "example.invalid",
        "private",
        "hidden",
        "alice@",
        "123e4567",
        "sk-supersecret",
        "abcdefghijklmnopqrstuvwxyzABCDEF",
        capability.as_str(),
    ] {
        assert!(
            !safe.contains(secret),
            "a planted diagnostic secret survived"
        );
    }
    for raw in [
        "https://example.invalid/private",
        "C:\\private\\data",
        "../hidden",
        "alice@example.invalid",
        "123e4567-e89b-42d3-a456-426614174000",
        "sk-AbCdEf",
        "abcdefghijklmnopqrstuvwxyzABCDEF",
    ] {
        let mut reply = AssistantReply::default();
        reply.push("Diagnostic ");
        reply.push(raw);
        let safe = reply.scrubbed();
        assert_eq!(safe, "Diagnostic [redacted]");
    }
    let mut bounded = AssistantReply::default();
    bounded.push(&"日本語 explanation ".repeat(1000));
    assert!(bounded.text.len() <= INPUT_BYTES);
    assert!(bounded.truncated);
    assert!(bounded.scrubbed().len() <= OUTPUT_BYTES);
    assert!(
        bounded
            .scrubbed()
            .is_char_boundary(bounded.scrubbed().len())
    );
}

#[test]
fn credential_markers_with_separators_withhold_following_values() {
    for raw in [
        "api_key = ALPHASECRET",
        "token ALPHASECRET",
        "Authorization : Bearer ALPHASECRET",
        "password\n=\nALPHASECRET",
    ] {
        let mut reply = AssistantReply::default();
        reply.push("Cannot create app. ");
        for part in raw.as_bytes().chunks(3) {
            reply.push(std::str::from_utf8(part).unwrap());
        }
        let safe = reply.scrubbed();
        assert!(safe.starts_with("Cannot create app"));
        assert!(!safe.contains("ALPHASECRET"));
    }
}
#[test]
fn synthetic_activity_summary_uses_known_names_closed_status_and_unique_updates_only() {
    let mut summary = ActivitySummary::default();
    let mut activity:ToolActivity=serde_json::from_value(serde_json::json!({"id":koru_ai_core::ToolInvocationId::new(),"attempt_id":koru_ai_core::InferenceAttemptId::new(),"chat_id":koru_ai_core::ChatId::new(),"assistant_message_id":koru_ai_core::MessageId::new(),"tool_name":"agent.mcp","agent_label":format!("koru_desktop.{}",mini_apps::LIST_TOOL),"read_only":true,"status":"running","approval":"not_required","proposed_at":koru_ai_core::Timestamp::now()})).unwrap();
    summary.observe(&activity);
    summary.observe(&activity);
    activity.status = ToolActivityStatus::Complete;
    summary.observe(&activity);
    assert_eq!(
        summary.values[mini_apps::LIST_TOOL].0,
        [0, 0, 1, 1, 0, 0, 0]
    );
    activity.agent_label = Some("foreign.SECRET_SENTINEL".into());
    activity.tool_name = "SECRET_SENTINEL".into();
    summary.observe(&activity);
    assert_eq!(summary.values.len(), 1);
}
