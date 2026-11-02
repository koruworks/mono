// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;

/// A `read_document` result as Koru's MCP server shapes it: the document's
/// node, then `blocks` paragraphs and tasks under it.
fn read_document(blocks: usize) -> Value {
    let root = "00000000-0000-4000-8000-000000000001";
    let mut nodes = vec![json!({
        "node_id": root,
        "parent_id": null,
        "ordinal": 0,
        "depth": 0,
        "kind": "document",
        "metadata": {"icon": null, "name": null, "colour": null, "done": null},
        "content": {"type": "text", "text": "Trip to Portugal", "attributes": [], "runs": []},
    })];
    for at in 0..blocks {
        let kind = if at % 10 == 0 { "task" } else { "paragraph" };
        nodes.push(json!({
            "node_id": format!("10000000-0000-4000-8000-{at:012}"),
            "parent_id": root,
            "ordinal": at,
            "depth": 1,
            "kind": kind,
            "metadata": {"icon": null, "name": null, "colour": null, "done": false},
            "content": {
                "type": "text",
                "text": format!("Day {at}: walk from Alfama to Belém, lunch by the river, then the tram back up the hill."),
                "attributes": [],
                "runs": [{"start": 0, "end": 4, "marks": ["bold"]}],
            },
        }));
    }
    json!({
        "document_id": "20000000-0000-4000-8000-000000000002",
        "root_node_id": root,
        "revision": "rev-42",
        "schema_version": 3,
        "nodes": nodes,
    })
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

#[test]
fn a_large_document_read_is_stored_as_a_summary_that_parses() {
    let result = serde_json::to_string(&read_document(200)).unwrap();
    assert!(
        result.len() > 50_000,
        "the fixture is {} bytes",
        result.len()
    );

    let stored = bound_tool_output(&result).expect("a summary");
    assert!(
        chars(&stored) <= TOOL_CONTENT_CHARS,
        "{} chars",
        chars(&stored)
    );
    let summary: Value = serde_json::from_str(&stored).expect("the summary is JSON");

    assert_eq!(
        summary["document_id"],
        "20000000-0000-4000-8000-000000000002"
    );
    assert_eq!(summary["revision"], "rev-42");
    // The title is the document node's text, kept first.
    assert_eq!(summary["nodes"][0]["kind"], "document");
    assert_eq!(summary["nodes"][0]["content"]["text"], "Trip to Portugal");
    // Only the first few nodes, but every one counted.
    let kept = summary["nodes"].as_array().unwrap().len();
    assert!((2..=5).contains(&kept), "{kept} nodes kept");
    let marker = &summary[TRUNCATED_KEY];
    assert_eq!(marker["bytes"], result.len());
    assert_eq!(marker["lengths"]["nodes"], 201);
    assert_eq!(marker["counts"]["blocks"], 200);
    assert_eq!(marker["counts"]["tasks"], 20);
}

#[test]
fn an_mcp_envelope_is_unwrapped_before_it_is_summarized() {
    // What Codex relays: the result as text, and again as structured content.
    let document = read_document(200);
    let envelope = json!({
        "content": [{"type": "text", "text": serde_json::to_string(&document).unwrap()}],
        "structuredContent": document,
    });
    let stored = bound_tool_output(&serde_json::to_string(&envelope).unwrap()).unwrap();
    assert!(chars(&stored) <= TOOL_CONTENT_CHARS);
    let summary: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(summary["nodes"][0]["content"]["text"], "Trip to Portugal");
    assert_eq!(summary[TRUNCATED_KEY]["counts"]["blocks"], 200);
}

#[test]
fn an_envelope_whose_result_fits_keeps_the_result_whole() {
    // The envelope doubles the result past the bound; the result alone fits.
    let document = read_document(6);
    let text = serde_json::to_string(&document).unwrap();
    assert!(chars(&text) <= TOOL_CONTENT_CHARS, "{} chars", chars(&text));
    let envelope = json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": document,
    });
    let envelope = serde_json::to_string(&envelope).unwrap();
    assert!(chars(&envelope) > TOOL_CONTENT_CHARS);
    assert_eq!(bound_tool_output(&envelope), Some(text));
}

#[test]
fn a_fenced_result_is_summarized_too() {
    let result = serde_json::to_string_pretty(&read_document(200)).unwrap();
    let stored = bound_tool_output(&format!("```json\n{result}\n```")).unwrap();
    let summary: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(summary[TRUNCATED_KEY]["lengths"]["nodes"], 201);
}

#[test]
fn a_summary_always_fits_however_wide_the_result() {
    // Hundreds of long top-level fields: every budget's summary is too big
    // until the last, and that one still parses.
    let wide: Map<String, Value> = (0..400)
        .map(|at| {
            (
                format!("field_{at:04}_{}", "x".repeat(60)),
                json!("y".repeat(200)),
            )
        })
        .collect();
    let stored = bound_tool_output(&serde_json::to_string(&wide).unwrap()).unwrap();
    assert!(chars(&stored) <= TOOL_CONTENT_CHARS);
    let summary: Value = serde_json::from_str(&stored).unwrap();
    assert!(summary[TRUNCATED_KEY]["bytes"].as_u64().unwrap() > 100_000);
}

#[test]
fn long_text_is_cut_on_a_character_boundary() {
    // Two- and four-byte characters straddle every byte offset near the cut.
    let text = "é🙂".repeat(TOOL_CONTENT_CHARS);
    let stored = bound_tool_output(&text).unwrap();
    assert_eq!(chars(&stored), TOOL_CONTENT_CHARS);
    assert!(text.starts_with(&stored));

    // JSON that was cut before it arrived is text, and is cut the same way.
    let broken = format!("{{\"text\": \"{}", "ü".repeat(TOOL_CONTENT_CHARS));
    let stored = bound_tool_output(&broken).unwrap();
    assert_eq!(chars(&stored), TOOL_CONTENT_CHARS);
    assert!(broken.starts_with(&stored));
}

#[test]
fn small_output_is_untouched() {
    let result = serde_json::to_string(&read_document(3)).unwrap();
    assert_eq!(bound_tool_output(&result), Some(result.clone()));
    assert_eq!(bound_tool_output("  total 0\n"), Some("total 0".to_owned()));
    assert_eq!(bound_tool_output("   "), None);
    // At the bound exactly, in characters rather than bytes.
    let full = "ß".repeat(TOOL_CONTENT_CHARS);
    assert_eq!(bound_tool_output(&full), Some(full.clone()));
}
