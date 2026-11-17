// SPDX-License-Identifier: MPL-2.0

//! The client, driven with the fake. No test here opens a socket: a request
//! is a function call into [`crate::fake`].
//!
//! The sections follow the list the plan is verified by: each tool;
//! `tool_not_available`; partial coverage; a claim held on the same
//! connection without the handle; saved, not saved, partially saved, unknown;
//! an expired token; a revoked connection; an ended subscription; a refused
//! revision.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::fake::{Access, FakeCloud, FakeConnection, Fate};
use crate::{
    Client, Coverage, EffectKind, EffectState, HostedTool, InvalidCall, NameStatus, NotSaved,
    PROTOCOL_REVISION, PartiallySaved, ReadTool, Recovery, Refusal, Saved, SearchCoverage, Unknown,
    UnknownCause, WriteOutcome, WriteTool,
};

// ------------------------------------------------------------------ helpers

struct Desk {
    cloud: FakeCloud,
    connection: Arc<FakeConnection>,
    client: Client,
    workspace: String,
}

/// A server, one Read & edit connection and its client.
fn desk() -> Desk {
    let cloud = FakeCloud::new();
    let connection = cloud.connect("first", Access::ReadAndEdit);
    Desk {
        client: connection.client(),
        workspace: cloud.workspace_id(),
        connection,
        cloud,
    }
}

/// What `read_document` answered: the revision, and the first paragraph's id
/// and text.
struct Page {
    revision: String,
    line: String,
    text: String,
    answer: Value,
}

fn read_page(client: &Client, workspace: &str, document: &str) -> Page {
    let read = client
        .read(
            ReadTool::ReadDocument,
            json!({ "workspace_id": workspace, "document_id": document }),
        )
        .expect("the page reads");
    let answer = read.answer().expose().clone();
    let paragraph = answer["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["kind"] == "paragraph")
        .expect("a paragraph")
        .clone();
    Page {
        revision: answer["revision"].as_str().expect("a revision").to_owned(),
        line: paragraph["node_id"].as_str().expect("a node id").to_owned(),
        text: paragraph["content"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        answer,
    }
}

/// Replace the first paragraph's text, as read just now.
fn rewrite(
    client: &Client,
    workspace: &str,
    document: &str,
    text: &str,
    claim_ids: &[&str],
) -> WriteOutcome {
    let page = read_page(client, workspace, document);
    let mut arguments = json!({
        "workspace_id": workspace,
        "document_id": document,
        "base_revision": page.revision,
        "operations": [{
            "op": "replace_text",
            "node_id": page.line,
            "expected": page.text,
            "text": text,
        }],
    });
    if !claim_ids.is_empty() {
        arguments["claim_ids"] = json!(claim_ids);
    }
    client.write(WriteTool::EditDocument, arguments)
}

fn create(client: &Client, workspace: &str, title: &str) -> WriteOutcome {
    client.write(
        WriteTool::CreateDocument,
        json!({
            "workspace_id": workspace,
            "title": title,
            "initial_operations": [
                { "op": "insert", "parent_id": "root", "kind": "paragraph", "text": "First line" },
            ],
        }),
    )
}

#[track_caller]
fn saved(outcome: WriteOutcome) -> Saved {
    match outcome {
        WriteOutcome::Saved(saved) => saved,
        other => panic!("expected saved, got {other:?}"),
    }
}

#[track_caller]
fn not_saved(outcome: WriteOutcome) -> NotSaved {
    match outcome {
        WriteOutcome::NotSaved(not_saved) => not_saved,
        other => panic!("expected not saved, got {other:?}"),
    }
}

#[track_caller]
fn partially_saved(outcome: WriteOutcome) -> PartiallySaved {
    match outcome {
        WriteOutcome::PartiallySaved(partial) => partial,
        other => panic!("expected partially saved, got {other:?}"),
    }
}

#[track_caller]
fn unknown(outcome: WriteOutcome) -> Unknown {
    match outcome {
        WriteOutcome::Unknown(unknown) => unknown,
        other => panic!("expected unknown, got {other:?}"),
    }
}

/// The tool's own code, for a refusal that is a tool's.
#[track_caller]
fn tool_code(refusal: &Refusal) -> &str {
    match refusal {
        Refusal::Tool(tool) => tool.code(),
        other => panic!("expected a tool's refusal, got {other:?}"),
    }
}

fn states(effects: &[crate::Effect]) -> Vec<(EffectKind, EffectState)> {
    effects
        .iter()
        .map(|effect| (effect.effect, effect.state))
        .collect()
}

// ----------------------------------------------------------------- catalog

#[test]
fn the_tool_enums_are_the_hosted_rung() {
    let names: Vec<&str> = ReadTool::ALL
        .into_iter()
        .map(ReadTool::name)
        .chain(WriteTool::ALL.into_iter().map(WriteTool::name))
        .collect();
    let mut ours = names.clone();
    ours.sort_unstable();
    let mut rung = koru_mcp_server::RUNG1_TOOLS.to_vec();
    rung.sort_unstable();
    assert_eq!(ours, rung, "a tool was added to or removed from the rung");

    let reads: Vec<&str> = ReadTool::ALL.into_iter().map(ReadTool::name).collect();
    assert_eq!(reads, koru_mcp_server::RUNG1_READ_TOOLS);

    for name in names {
        assert_eq!(
            HostedTool::from_name(name).map(HostedTool::name),
            Some(name)
        );
    }
    assert_eq!(HostedTool::from_name("update_task"), None);
}

#[test]
fn list_tools_follows_every_page_and_the_connections_access() {
    let desk = desk();
    desk.cloud.page_tools(4);
    let listed: Vec<String> = desk
        .client
        .list_tools()
        .expect("the catalog")
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(listed.len(), koru_mcp_server::RUNG1_TOOLS.len());
    for name in koru_mcp_server::RUNG1_TOOLS {
        assert!(
            listed.iter().any(|listed| listed == name),
            "{name} is not listed"
        );
    }
    assert_eq!(desk.cloud.requests(), 4, "fourteen tools, four at a time");

    // A hosted edit takes the handles of the claims in its way.
    let tools = desk.client.list_tools().unwrap();
    let edit = tools
        .iter()
        .find(|tool| tool.name == "edit_document")
        .unwrap();
    assert!(
        edit.definition().expose()["inputSchema"]["properties"]["claim_ids"].is_object(),
        "the hosted edit_document publishes claim_ids"
    );

    let reader = desk.cloud.connect("reader", Access::Read).client();
    let mut listed: Vec<String> = reader
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    listed.sort_unstable();
    let mut reads = koru_mcp_server::RUNG1_READ_TOOLS.to_vec();
    reads.sort_unstable();
    assert_eq!(listed, reads, "a Read connection is listed the reads only");
}

// --------------------------------------------------------------- each tool

#[test]
fn get_documentation_answers_the_contract() {
    let desk = desk();
    let read = desk
        .client
        .read(ReadTool::GetDocumentation, json!({}))
        .expect("the essentials");
    assert!(read.answer().expose()["text"].is_string());
    assert_eq!(read.coverage, Coverage::NotApplicable);
}

#[test]
fn list_workspaces_names_each_workspaces_profile() {
    let desk = desk();
    let listed = desk.client.list_workspaces().expect("the workspaces");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.to_string(), desk.workspace);
    assert!(
        listed[0].profile_id.is_some(),
        "the entry carries profile_id"
    );
    assert_eq!(listed[0].name_status, NameStatus::Available);

    desk.cloud.set_names_loading(true);
    let listed = desk.client.list_workspaces().unwrap();
    assert_eq!(listed[0].name_status, NameStatus::Loading);
    assert_eq!(
        listed[0].id.to_string(),
        desk.workspace,
        "the id does not wait for the name"
    );
}

#[test]
fn a_workspace_the_connection_does_not_list_is_refused() {
    let desk = desk();
    let elsewhere = uuid::Uuid::new_v4().to_string();
    let refusal = desk
        .client
        .read(
            ReadTool::ListDocuments,
            json!({ "workspace_id": elsewhere }),
        )
        .expect_err("another workspace");
    assert_eq!(tool_code(&refusal), "permission_denied");
    let outcome = desk.client.write(
        WriteTool::CreateDocument,
        json!({ "workspace_id": elsewhere, "title": "Elsewhere" }),
    );
    assert_eq!(tool_code(&not_saved(outcome).reason), "permission_denied");
}

#[test]
fn list_documents_and_read_document_answer_what_is_there() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let listed = desk
        .client
        .read(
            ReadTool::ListDocuments,
            json!({ "workspace_id": desk.workspace }),
        )
        .expect("the list");
    let documents = listed.answer().expose()["documents"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["id"], document);

    let page = read_page(&desk.client, &desk.workspace, &document);
    assert_eq!(page.text, "Buy milk");
    assert!(!page.revision.is_empty());
    assert!(
        page.answer.get("history").is_none(),
        "hosted answers carry no history"
    );
    assert!(
        page.answer.get("claim").is_none(),
        "an unclaimed document shows no claim"
    );
}

#[test]
fn create_document_is_saved_with_a_revision_and_the_key_it_went_out_with() {
    let desk = desk();
    let made = saved(create(&desk.client, &desk.workspace, "Field notes"));
    let document = made.document_id.expect("the new document").to_string();
    assert!(made.revision.is_some(), "a success carries a revision");
    let key = made.idempotency_key.expect("the key");
    assert_eq!(
        made.answer().expose()["idempotency_key"],
        key.to_string(),
        "the key the client minted is the one the server answered"
    );
    assert!(made.answer().expose().get("history").is_none());
    assert!(
        made.answer().expose().get("operation_id").is_none(),
        "a success carries no operation id"
    );
    assert!(desk.cloud.lists(&document));
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "First line"
    );

    // A caller's own key goes out unchanged and comes back.
    let own = uuid::Uuid::new_v4();
    let made = saved(desk.client.write(
        WriteTool::CreateDocument,
        json!({ "workspace_id": desk.workspace, "title": "Keyed", "idempotency_key": own.to_string() }),
    ));
    assert_eq!(made.idempotency_key, Some(own));
}

#[test]
fn edit_document_is_saved_with_the_new_revision() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let before = read_page(&desk.client, &desk.workspace, &document).revision;
    let edit = saved(rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Buy oat milk",
        &[],
    ));
    let revision = edit.revision.as_ref().expect("a revision").as_str().to_owned();
    assert_ne!(revision, before);
    assert!(
        edit.answer().expose().get("history").is_none(),
        "hosted answers carry no history"
    );
    let after = read_page(&desk.client, &desk.workspace, &document);
    assert_eq!(after.text, "Buy oat milk");
    assert_eq!(
        after.revision, revision,
        "the answered revision is the one a read sees"
    );
}

fn extract(
    client: &Client,
    workspace: &str,
    source: &str,
    destination: Result<&str, &str>,
) -> WriteOutcome {
    let page = read_page(client, workspace, source);
    let mut arguments = json!({
        "workspace_id": workspace,
        "source_document_id": source,
        "base_revision": page.revision,
        "source_node_id": page.line,
        "expected": page.text,
    });
    match destination {
        Ok(existing) => arguments["target_document_id"] = json!(existing),
        Err(title) => arguments["new_document_title"] = json!(title),
    }
    client.write(WriteTool::ExtractToDocument, arguments)
}

#[test]
fn extract_to_document_is_saved_with_both_revisions() {
    let desk = desk();
    let source = desk.cloud.add_page("Source", "Moved across");
    let destination = desk.cloud.add_page("Destination", "");
    let moved = saved(extract(
        &desk.client,
        &desk.workspace,
        &source,
        Ok(&destination),
    ));
    assert_eq!(
        moved.document_id.map(|id| id.to_string()),
        Some(destination.clone())
    );
    assert!(moved.revision.is_some() && moved.source_revision.is_some());
    let landed = read_page(&desk.client, &desk.workspace, &destination).answer;
    assert!(
        landed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["content"]["text"] == "Moved across"),
        "the content is in the destination"
    );

    // Into a page the call makes.
    let source = desk.cloud.add_page("Another source", "Moved again");
    let moved = saved(extract(
        &desk.client,
        &desk.workspace,
        &source,
        Err("Made by the move"),
    ));
    let made = moved.document_id.expect("the new page").to_string();
    assert!(desk.cloud.lists(&made));
}

#[test]
fn the_image_tools_answer_that_uploads_are_not_available() {
    // `contracts/hosted-tools-v1.md` section 3, "Image uploads": the byte
    // route is not implemented by an in-process replica in this repository,
    // so "`create_image_upload` answers `image_upload_unavailable` and the
    // place tools answer `image_upload_unavailable` or `claims_unavailable`.
    // Neither writes a document."
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let page = read_page(&desk.client, &desk.workspace, &document);
    let requests = desk.cloud.requests();

    let upload = desk.client.write(
        WriteTool::CreateImageUpload,
        json!({ "workspace_id": desk.workspace, "name": "lens.png" }),
    );
    assert_eq!(
        tool_code(&not_saved(upload).reason),
        "image_upload_unavailable"
    );

    let add = desk.client.write(
        WriteTool::AddImage,
        json!({
            "workspace_id": desk.workspace,
            "document_id": document,
            "base_revision": page.revision,
            "upload_id": "none",
            "parent_id": page.answer["root_node_id"],
        }),
    );
    assert!(
        ["image_upload_unavailable", "claims_unavailable"]
            .contains(&tool_code(&not_saved(add).reason)),
    );

    let replace = desk.client.write(
        WriteTool::ReplaceImage,
        json!({
            "workspace_id": desk.workspace,
            "document_id": document,
            "base_revision": page.revision,
            "upload_id": "none",
            "node_id": page.line,
        }),
    );
    assert!(
        ["image_upload_unavailable", "claims_unavailable"]
            .contains(&tool_code(&not_saved(replace).reason)),
    );

    assert_eq!(desk.cloud.requests(), requests + 3, "one request each");
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).revision,
        page.revision,
        "neither wrote the document"
    );
}

#[test]
fn claim_document_and_release_document_answer_the_handle() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let taken = saved(desk.client.write(
        WriteTool::ClaimDocument,
        json!({ "workspace_id": desk.workspace, "document_id": document, "purpose": "Rewriting" }),
    ));
    let grant = taken.claim.expect("the grant");
    assert!(!grant.handle.expose().is_empty());
    assert!(taken.revision.is_none(), "a claim writes no document");

    let released = saved(desk.client.write(
        WriteTool::ReleaseDocument,
        json!({ "claim_id": grant.handle.expose() }),
    ));
    assert_eq!(
        released.answer().expose()["claim_id"],
        grant.handle.expose()
    );
    // "Idempotent: releasing a claim that is already released ... answers
    // the same."
    saved(desk.client.write(
        WriteTool::ReleaseDocument,
        json!({ "claim_id": grant.handle.expose() }),
    ));
}

// ------------------------------------------------------ tool_not_available

#[test]
fn a_tool_outside_the_hosted_catalog_is_tool_not_available() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    // Tools the desktop serves and the hosted rung does not: versions,
    // files, the task command.
    for name in [
        "list_document_versions",
        "save_document_version",
        "update_task",
        "list_files",
    ] {
        let before = desk.cloud.requests();
        let outcome = desk.client.call_unlisted(
            name,
            json!({ "workspace_id": desk.workspace, "document_id": document }),
        );
        assert_eq!(
            not_saved(outcome).reason,
            Refusal::ToolNotAvailable,
            "{name}"
        );
        assert_eq!(desk.cloud.requests(), before + 1, "{name} was asked once");
    }

    // A name the catalog does have goes through `read` or `write`, where
    // its answer has a meaning; here it is refused before anything is sent.
    let before = desk.cloud.requests();
    let outcome = desk.client.call_unlisted("edit_document", json!({}));
    assert_eq!(
        not_saved(outcome).reason,
        Refusal::InvalidCall(InvalidCall::ToolName)
    );
    let outcome = desk.client.call_unlisted("Not A Name", json!({}));
    assert_eq!(
        not_saved(outcome).reason,
        Refusal::InvalidCall(InvalidCall::ToolName)
    );
    assert_eq!(desk.cloud.requests(), before);
}

// --------------------------------------------------------- partial coverage

#[test]
fn a_search_says_how_much_content_it_looked_in() {
    let desk = desk();
    let alpha = desk.cloud.add_page("Alpha", "the needle is here");
    let beta = desk.cloud.add_page("Beta", "a needle here too");
    let search = |query: &str| {
        let read = desk
            .client
            .read(
                ReadTool::SearchDocuments,
                json!({ "query": query, "workspace_ids": [desk.workspace] }),
            )
            .expect("a search");
        let hits = read.answer().expose()["hits"].as_array().unwrap().len();
        (read.coverage, hits)
    };
    let counts = |searched, eligible| SearchCoverage {
        content_documents_searched: searched,
        eligible_documents: eligible,
    };

    // Nothing opened: titles are searched everywhere, content nowhere. No
    // hit says nothing about the content of either page.
    assert_eq!(search("needle"), (Coverage::Partial(counts(0, 2)), 0));
    assert_eq!(
        search("alpha"),
        (Coverage::Partial(counts(0, 2)), 1),
        "a title is found"
    );

    read_page(&desk.client, &desk.workspace, &alpha);
    assert_eq!(search("needle"), (Coverage::Partial(counts(1, 2)), 1));

    read_page(&desk.client, &desk.workspace, &beta);
    assert_eq!(search("needle"), (Coverage::Complete(counts(2, 2)), 2));

    // Another connection has opened nothing: coverage is the connection's.
    let other = desk.cloud.connect("second", Access::Read).client();
    let read = other
        .read(ReadTool::SearchDocuments, json!({ "query": "needle" }))
        .unwrap();
    assert_eq!(read.coverage, Coverage::Partial(counts(0, 2)));
}

#[test]
fn list_tasks_refuses_until_every_document_has_been_opened() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let refusal = desk
        .client
        .read(ReadTool::ListTasks, json!({}))
        .expect_err("a partial task list would look complete");
    let Refusal::Tool(tool) = &refusal else {
        panic!("expected the tool's refusal, got {refusal:?}");
    };
    assert_eq!(tool.code(), "task_index_incomplete");
    assert_eq!(
        tool.coverage,
        Some(SearchCoverage {
            content_documents_searched: 0,
            eligible_documents: 1
        })
    );

    read_page(&desk.client, &desk.workspace, &document);
    let read = desk
        .client
        .read(ReadTool::ListTasks, json!({}))
        .expect("every body is open");
    assert!(read.answer().expose()["tasks"].is_array());
}

// ------------------------------------------------------------------- claims

fn claim(client: &Client, workspace: &str, document: &str) -> WriteOutcome {
    client.write(
        WriteTool::ClaimDocument,
        json!({ "workspace_id": workspace, "document_id": document, "agent_label": "Drafter" }),
    )
}

#[test]
fn a_claim_refuses_the_same_connection_without_the_handle() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    // Two Bots on one server share one connection: two clients, one
    // connection.
    let first = desk.connection.client();
    let second = desk.connection.client();
    let handle = saved(claim(&first, &desk.workspace, &document))
        .claim
        .expect("the grant")
        .handle;

    // The other Bot, on the same connection, without the handle.
    let refused = not_saved(rewrite(
        &second,
        &desk.workspace,
        &document,
        "Mine now",
        &[],
    ));
    let Refusal::Tool(tool) = &refused.reason else {
        panic!("expected the tool's refusal, got {:?}", refused.reason);
    };
    assert_eq!(tool.code(), "document_claimed");
    let claimed = tool.claimed.expect("what is in the way");
    assert_eq!(
        claimed.document_id.map(|id| id.to_string()),
        Some(document.clone())
    );
    assert_eq!(claimed.retry_after_seconds, Some(300));
    assert!(claimed.expires_at.is_some());
    assert_eq!(
        read_page(&second, &desk.workspace, &document).text,
        "Buy milk",
        "nothing was written"
    );

    // A wrong handle is no handle.
    let refused = not_saved(rewrite(
        &second,
        &desk.workspace,
        &document,
        "Mine now",
        &["not-the-handle"],
    ));
    assert_eq!(tool_code(&refused.reason), "document_claimed");

    // Claiming it again, on the same connection, is under another handle.
    let refused = not_saved(claim(&second, &desk.workspace, &document));
    assert_eq!(tool_code(&refused.reason), "document_claimed");

    // The holder, with the handle.
    saved(rewrite(
        &first,
        &desk.workspace,
        &document,
        "Buy oat milk",
        &[handle.expose()],
    ));
    assert_eq!(
        read_page(&first, &desk.workspace, &document).text,
        "Buy oat milk"
    );

    // A handle for a document the call does not change is ignored.
    let other = desk.cloud.add_page("Other", "Untouched");
    saved(rewrite(
        &second,
        &desk.workspace,
        &other,
        "Touched",
        &[handle.expose()],
    ));
}

#[test]
fn a_claim_refuses_other_connections_and_is_shown_on_reads() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let other = desk.cloud.connect("second", Access::ReadAndEdit).client();
    let handle = saved(claim(&desk.client, &desk.workspace, &document))
        .claim
        .unwrap()
        .handle;

    // Even with the handle, from a connection that does not hold the claim.
    let refused = not_saved(rewrite(
        &other,
        &desk.workspace,
        &document,
        "Theirs",
        &[handle.expose()],
    ));
    assert_eq!(tool_code(&refused.reason), "document_claimed");

    // An extraction names which of its two documents is claimed.
    let source = desk.cloud.add_page("Source", "Moved across");
    let refused = not_saved(extract(&other, &desk.workspace, &source, Ok(&document)));
    let Refusal::Tool(tool) = &refused.reason else {
        panic!("expected the tool's refusal");
    };
    assert_eq!(
        tool.claimed.unwrap().document_id.map(|id| id.to_string()),
        Some(document.clone())
    );

    // Reads are never refused, and show the claim.
    let page = read_page(&other, &desk.workspace, &document);
    assert_eq!(page.answer["claim"]["holder"]["connection_name"], "first");
    assert_eq!(page.answer["claim"]["holder"]["agent_label"], "Drafter");
    let listed = other
        .read(
            ReadTool::ListDocuments,
            json!({ "workspace_id": desk.workspace }),
        )
        .unwrap();
    let entries = listed.answer().expose()["documents"]
        .as_array()
        .unwrap()
        .clone();
    let entry = entries
        .iter()
        .find(|entry| entry["id"] == document)
        .unwrap();
    assert!(entry["claim"]["expires_at"].is_i64());

    // Released, the document is anyone's again.
    saved(desk.client.write(
        WriteTool::ReleaseDocument,
        json!({ "claim_id": handle.expose() }),
    ));
    saved(rewrite(&other, &desk.workspace, &document, "Theirs", &[]));
    assert!(
        read_page(&other, &desk.workspace, &document)
            .answer
            .get("claim")
            .is_none()
    );
}

#[test]
fn a_claim_expires_renews_and_is_limited() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let other = desk.cloud.connect("second", Access::ReadAndEdit).client();
    let taken = saved(claim(&desk.client, &desk.workspace, &document))
        .claim
        .unwrap();
    let renew = |handle: &str| {
        desk.client.write(
            WriteTool::ClaimDocument,
            json!({ "workspace_id": desk.workspace, "document_id": document, "claim_id": handle }),
        )
    };

    // Renewing keeps the handle and moves the expiry.
    desk.cloud.advance_seconds(200);
    let renewed = saved(renew(taken.handle.expose())).claim.unwrap();
    assert_eq!(renewed.handle.expose(), taken.handle.expose());
    assert_eq!(renewed.expires_at, taken.expires_at + 200_000);

    // A handle the server does not hold.
    assert_eq!(
        tool_code(&not_saved(renew("never-minted")).reason),
        "claim_not_found"
    );

    // Expired: other agents may change the document, and the handle is gone.
    desk.cloud.advance_seconds(301);
    saved(rewrite(&other, &desk.workspace, &document, "Theirs", &[]));
    assert_eq!(
        tool_code(&not_saved(renew(taken.handle.expose())).reason),
        "claim_expired"
    );

    // A duration outside 60 to 900 seconds is refused.
    let refused = not_saved(desk.client.write(
        WriteTool::ClaimDocument,
        json!({ "workspace_id": desk.workspace, "document_id": document, "duration_seconds": 30 }),
    ));
    assert_eq!(tool_code(&refused.reason), "invalid_request");

    // "the connection already holds 16 live claims".
    for index in 0..16 {
        let page = desk.cloud.add_page(&format!("Page {index}"), "");
        saved(claim(&desk.client, &desk.workspace, &page));
    }
    let one_more = desk.cloud.add_page("One more", "");
    assert_eq!(
        tool_code(&not_saved(claim(&desk.client, &desk.workspace, &one_more)).reason),
        "claim_limit_reached"
    );
    // The limit is the connection's, not the server's.
    saved(claim(&other, &desk.workspace, &one_more));
}

// --------------------------------------------- saved, not saved, partial, unknown

#[test]
fn a_refused_edit_is_not_saved_and_changes_nothing() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let elsewhere = desk.cloud.add_page("Elsewhere", "Another history");
    let page = read_page(&desk.client, &desk.workspace, &document);
    let foreign = read_page(&desk.client, &desk.workspace, &elsewhere).revision;

    // A base revision this document never had: `revision_conflict`, which is
    // the refusal's own code and so "all `not_saved`".
    let outcome = desk.client.write(
        WriteTool::EditDocument,
        json!({
            "workspace_id": desk.workspace,
            "document_id": document,
            "base_revision": foreign,
            "operations": [{
                "op": "replace_text", "node_id": page.line, "expected": page.text, "text": "Overwritten",
            }],
        }),
    );
    assert_eq!(outcome.code(), "not_saved");
    assert_eq!(tool_code(&not_saved(outcome).reason), "revision_conflict");

    // Koru Cloud refuses the one submission: `edit_not_saved`.
    desk.cloud.arm(0, Fate::NotSaved);
    let outcome = not_saved(rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Overwritten",
        &[],
    ));
    assert_eq!(tool_code(&outcome.reason), "edit_not_saved");

    let after = read_page(&desk.client, &desk.workspace, &document);
    assert_eq!(
        (after.text.as_str(), after.revision),
        ("Buy milk", page.revision)
    );

    // The script was for one call: the next edit saves.
    saved(rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Buy oat milk",
        &[],
    ));
}

#[test]
fn a_write_that_never_left_is_not_saved() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let page = read_page(&desk.client, &desk.workspace, &document);
    let requests = desk.cloud.requests();
    desk.connection.fail_before_sending();
    let outcome = desk.client.write(
        WriteTool::EditDocument,
        json!({
            "workspace_id": desk.workspace,
            "document_id": document,
            "base_revision": page.revision,
            "operations": [{ "op": "replace_text", "node_id": page.line, "expected": page.text, "text": "Lost" }],
        }),
    );
    assert_eq!(not_saved(outcome).reason, Refusal::NotSent);
    assert_eq!(
        desk.cloud.requests(),
        requests,
        "nothing reached the server"
    );

    // Arguments that are not an object are refused before a token is asked for.
    let outcome = desk.client.write(WriteTool::EditDocument, json!("edit it"));
    assert_eq!(
        not_saved(outcome).reason,
        Refusal::InvalidCall(InvalidCall::ArgumentsNotAnObject)
    );
    let outcome = desk.client.write(
        WriteTool::CreateDocument,
        json!({ "workspace_id": desk.workspace, "title": "Keyed", "idempotency_key": "tuesday" }),
    );
    assert_eq!(
        not_saved(outcome).reason,
        Refusal::InvalidCall(InvalidCall::IdempotencyKeyNotAUuid)
    );
    assert_eq!(desk.cloud.requests(), requests);
}

#[test]
fn a_creation_saved_but_not_listed_is_partially_saved_and_finished_with_the_same_key() {
    let desk = desk();
    // `create_document`: `document`, then `manifest`. The entry is refused.
    desk.cloud.arm(1, Fate::NotSaved);
    let outcome = create(&desk.client, &desk.workspace, "Field notes");
    assert_eq!(outcome.code(), "partially_saved");
    let partial = partially_saved(outcome);
    assert_eq!(
        states(&partial.effects),
        [
            (EffectKind::Document, EffectState::Saved),
            (EffectKind::Manifest, EffectState::NotSaved),
        ]
    );
    let document = partial.effects[0]
        .document_id
        .expect("the saved document")
        .to_string();
    assert!(
        !desk.cloud.lists(&document),
        "the document exists and is not listed"
    );
    let Recovery::FinishCreate(pending) = &partial.recovery else {
        panic!(
            "a creation is finished with its key, got {:?}",
            partial.recovery
        );
    };
    assert_eq!(
        partial.answer().expose()["details"]["idempotency_key"],
        pending.idempotency_key.to_string(),
        "the envelope names the key the client sent"
    );

    let finished = saved(desk.client.finish_create(pending));
    assert_eq!(
        finished.document_id.map(|id| id.to_string()),
        Some(document.clone())
    );
    assert_eq!(finished.idempotency_key, Some(pending.idempotency_key));
    assert!(desk.cloud.lists(&document));

    // Finishing twice is the same document, not a second one.
    let again = saved(desk.client.finish_create(pending));
    assert_eq!(again.document_id, finished.document_id);
}

#[test]
fn an_extraction_whose_source_was_not_replaced_is_partially_saved_and_not_repeated() {
    let desk = desk();
    let source = desk.cloud.add_page("Source", "Moved across");
    let destination = desk.cloud.add_page("Destination", "");
    // Into an existing page: `destination`, then `source`. The second is
    // refused.
    desk.cloud.arm(1, Fate::NotSaved);
    let partial = partially_saved(extract(
        &desk.client,
        &desk.workspace,
        &source,
        Ok(&destination),
    ));
    assert_eq!(
        states(&partial.effects),
        [
            (EffectKind::Destination, EffectState::Saved),
            (EffectKind::Source, EffectState::NotSaved),
        ]
    );
    assert_eq!(
        partial.recovery,
        Recovery::ReadFirst,
        "only a creation has a key"
    );
    // "the content is in both documents"; the recovery fields say so.
    assert_eq!(
        partial.answer().expose()["details"]["source_unchanged"],
        true
    );
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &source).text,
        "Moved across"
    );
}

#[test]
fn an_edit_the_server_could_not_confirm_is_unknown_and_is_never_sent_again() {
    let desk = desk();
    for (fate, landed) in [
        (Fate::UnknownAndLost, false),
        (Fate::UnknownButLanded, true),
    ] {
        let document = desk.cloud.add_page("Plan", "Buy milk");
        let page = read_page(&desk.client, &desk.workspace, &document);
        desk.cloud.arm(0, fate);
        let before = desk.cloud.requests();
        let outcome = desk.client.write(
            WriteTool::EditDocument,
            json!({
                "workspace_id": desk.workspace,
                "document_id": document,
                "base_revision": page.revision,
                "operations": [{
                    "op": "replace_text", "node_id": page.line, "expected": "Buy milk", "text": "Buy oat milk",
                }],
            }),
        );
        assert_eq!(
            desk.cloud.requests(),
            before + 1,
            "one request, and no second"
        );
        assert_eq!(outcome.code(), "unknown");
        let unknown = unknown(outcome);
        // The two cases are the same answer. Only a read tells them apart.
        assert_eq!(unknown.cause, UnknownCause::ServerCouldNotConfirm);
        assert!(unknown.operation_id.is_some());
        assert_eq!(
            states(&unknown.effects),
            [(EffectKind::Document, EffectState::Unknown)]
        );
        assert_eq!(
            unknown.effects[0].document_id.map(|id| id.to_string()),
            Some(document.clone())
        );
        assert_eq!(
            unknown.recovery,
            Recovery::ReadFirst,
            "an edit has no key to repeat under"
        );

        let text = read_page(&desk.client, &desk.workspace, &document).text;
        assert_eq!(text == "Buy oat milk", landed);
    }
}

#[test]
fn a_write_whose_answer_never_came_back_is_unknown() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let page = read_page(&desk.client, &desk.workspace, &document);
    desk.connection.lose_next_answer();
    let before = desk.cloud.requests();
    let outcome = desk.client.write(
        WriteTool::EditDocument,
        json!({
            "workspace_id": desk.workspace,
            "document_id": document,
            "base_revision": page.revision,
            "operations": [{
                "op": "replace_text", "node_id": page.line, "expected": "Buy milk", "text": "Buy oat milk",
            }],
        }),
    );
    assert_eq!(desk.cloud.requests(), before + 1, "the request left once");
    let unknown = unknown(outcome);
    assert_eq!(unknown.cause, UnknownCause::NoAnswer);
    assert!(
        unknown.operation_id.is_none() && unknown.effects.is_empty() && unknown.answer().is_none()
    );
    assert_eq!(unknown.recovery, Recovery::ReadFirst);
    // It did save. The client could not have said so.
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy oat milk"
    );

    // A claim whose answer is lost is unknown too: the handle never arrived.
    desk.connection.lose_next_answer();
    let lost = unknown_of(claim(&desk.client, &desk.workspace, &document));
    assert_eq!(lost.recovery, Recovery::ReadFirst);
    assert!(
        read_page(&desk.client, &desk.workspace, &document)
            .answer
            .get("claim")
            .is_some(),
        "a read shows whether the document is claimed"
    );
}

#[track_caller]
fn unknown_of(outcome: WriteOutcome) -> Unknown {
    unknown(outcome)
}

#[test]
fn an_unknown_creation_is_finished_with_the_same_key_and_makes_one_document() {
    let listed = |desk: &Desk| {
        desk.client
            .read(
                ReadTool::ListDocuments,
                json!({ "workspace_id": desk.workspace }),
            )
            .unwrap()
            .answer()
            .expose()["documents"]
            .as_array()
            .unwrap()
            .len()
    };

    // The answer is lost after the creation landed.
    let desk = desk();
    desk.connection.lose_next_answer();
    let lost = unknown(create(&desk.client, &desk.workspace, "Field notes"));
    assert_eq!(lost.cause, UnknownCause::NoAnswer);
    let Recovery::FinishCreate(pending) = &lost.recovery else {
        panic!("a creation is finished with its key");
    };
    let finished = saved(desk.client.finish_create(pending));
    assert_eq!(finished.idempotency_key, Some(pending.idempotency_key));
    assert_eq!(listed(&desk), 1, "the replay made no second document");

    // The seed was submitted and never acknowledged, and did not land.
    desk.cloud.arm(0, Fate::UnknownAndLost);
    let lost = unknown(create(&desk.client, &desk.workspace, "Second"));
    assert_eq!(lost.cause, UnknownCause::ServerCouldNotConfirm);
    assert_eq!(
        states(&lost.effects),
        [
            (EffectKind::Document, EffectState::Unknown),
            (EffectKind::Manifest, EffectState::NotSaved),
        ]
    );
    let Recovery::FinishCreate(pending) = &lost.recovery else {
        panic!("a creation is finished with its key");
    };
    saved(desk.client.finish_create(pending));
    assert_eq!(listed(&desk), 2);

    // The entry was submitted and never acknowledged, and did land.
    desk.cloud.arm(1, Fate::UnknownButLanded);
    let lost = unknown(create(&desk.client, &desk.workspace, "Third"));
    assert_eq!(
        states(&lost.effects),
        [
            (EffectKind::Document, EffectState::Saved),
            (EffectKind::Manifest, EffectState::Unknown),
        ]
    );
    let document = lost.effects[0].document_id;
    let Recovery::FinishCreate(pending) = &lost.recovery else {
        panic!("a creation is finished with its key");
    };
    let finished = saved(desk.client.finish_create(pending));
    assert_eq!(finished.document_id, document);
    assert_eq!(listed(&desk), 3);
}

#[test]
fn the_same_key_with_a_different_request_creates_nothing() {
    let desk = desk();
    let key = uuid::Uuid::new_v4().to_string();
    let make = |title: &str| {
        desk.client.write(
            WriteTool::CreateDocument,
            json!({ "workspace_id": desk.workspace, "title": title, "idempotency_key": key }),
        )
    };
    let first = saved(make("Field notes"));
    let refused = not_saved(make("Something else"));
    assert_eq!(tool_code(&refused.reason), "idempotency_conflict");
    assert_eq!(
        refused.idempotency_key.map(|key| key.to_string()),
        Some(key.clone())
    );
    assert_eq!(
        saved(make("Field notes")).document_id,
        first.document_id,
        "the same request replays"
    );
}

// ------------------------------- token, connection, subscription, revision

/// One read, one write and the catalog, each refused the same way, each
/// asked once.
#[track_caller]
fn assert_everything_refused(desk: &Desk, document: &str, expected: &Refusal) {
    let before = desk.cloud.requests();
    let read = desk
        .client
        .read(
            ReadTool::ReadDocument,
            json!({ "workspace_id": desk.workspace, "document_id": document }),
        )
        .expect_err("a read is refused");
    assert_eq!(&read, expected);
    let write = not_saved(create(&desk.client, &desk.workspace, "Refused"));
    assert_eq!(&write.reason, expected);
    assert!(
        write.idempotency_key.is_some(),
        "the key is known even when nothing was sent"
    );
    assert_eq!(
        &desk
            .client
            .list_tools()
            .expect_err("the catalog is refused"),
        expected
    );
    assert_eq!(
        desk.cloud.requests(),
        before + 3,
        "each was asked once: a refusal is not retried"
    );
}

#[test]
fn an_expired_token_is_token_expired_until_it_is_renewed() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    desk.connection.expire_token();
    assert_everything_refused(&desk, &document, &Refusal::TokenExpired);
    assert_eq!(Refusal::TokenExpired.code(), "token_expired");

    // The client does not renew. The credential's owner does, and the next
    // call carries the new token.
    desk.connection.renew_token();
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );
}

#[test]
fn a_revoked_connection_is_connection_revoked_and_its_claims_are_released() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let other = desk.cloud.connect("second", Access::ReadAndEdit).client();
    saved(claim(&desk.client, &desk.workspace, &document));

    desk.connection.revoke();
    assert_everything_refused(&desk, &document, &Refusal::ConnectionRevoked);
    assert_eq!(Refusal::ConnectionRevoked.code(), "connection_revoked");
    // A new token changes nothing: the connection is what was revoked.
    desk.connection.renew_token();
    assert_everything_refused(&desk, &document, &Refusal::ConnectionRevoked);

    // The other connection is untouched, and the claim went with the revoked one.
    saved(rewrite(&other, &desk.workspace, &document, "Theirs", &[]));
}

#[test]
fn an_ended_subscription_is_subscription_ended_and_the_connection_survives_it() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    desk.cloud.set_subscription_ended(true);
    assert_everything_refused(&desk, &document, &Refusal::SubscriptionEnded);
    assert_eq!(Refusal::SubscriptionEnded.code(), "subscription_ended");

    desk.cloud.set_subscription_ended(false);
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );
}

#[test]
fn a_server_that_cannot_check_the_connection_is_authorization_unavailable() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    desk.cloud.set_authorization_unavailable(true);
    assert_everything_refused(&desk, &document, &Refusal::AuthorizationUnavailable);
    desk.cloud.set_authorization_unavailable(false);
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );
}

#[test]
fn a_refused_revision_is_revision_refused_with_what_the_server_speaks() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    // By default the fake speaks what HOST.md says the built server speaks,
    // this client's revision among them.
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );

    desk.cloud.speak_only(&["2025-11-25", "2025-06-18"]);
    let expected = Refusal::RevisionRefused {
        supported: vec!["2025-11-25".to_owned(), "2025-06-18".to_owned()],
    };
    assert_everything_refused(&desk, &document, &expected);
    assert_eq!(expected.code(), "revision_refused");
    assert!(
        !matches!(&expected, Refusal::RevisionRefused { supported } if supported.iter().any(|known| known == PROTOCOL_REVISION))
    );

    // The design's server: this revision and no other.
    desk.cloud.speak_only(&[PROTOCOL_REVISION]);
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );
}

#[test]
fn a_read_connection_is_refused_every_write_as_insufficient_scope() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    let reader = desk.cloud.connect("reader", Access::Read).client();
    assert_eq!(
        read_page(&reader, &desk.workspace, &document).text,
        "Buy milk"
    );
    for outcome in [
        create(&reader, &desk.workspace, "Refused"),
        rewrite(&reader, &desk.workspace, &document, "Refused", &[]),
        claim(&reader, &desk.workspace, &document),
    ] {
        assert_eq!(not_saved(outcome).reason, Refusal::InsufficientScope);
    }
    assert_eq!(
        read_page(&reader, &desk.workspace, &document).text,
        "Buy milk"
    );
}

#[test]
fn the_four_states_have_four_distinct_stable_codes() {
    let codes = [
        Refusal::TokenExpired.code(),
        Refusal::ConnectionRevoked.code(),
        Refusal::SubscriptionEnded.code(),
        Refusal::RevisionRefused {
            supported: Vec::new(),
        }
        .code(),
    ];
    assert_eq!(
        codes,
        [
            "token_expired",
            "connection_revoked",
            "subscription_ended",
            "revision_refused"
        ]
    );
}

// ---------------------------------------------------------------- the wire

#[test]
fn an_answer_sent_as_an_event_stream_reads_the_same() {
    let desk = desk();
    let document = desk.cloud.add_page("Plan", "Buy milk");
    desk.cloud.answer_as_event_stream(true);
    assert_eq!(
        read_page(&desk.client, &desk.workspace, &document).text,
        "Buy milk"
    );
    saved(rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Buy oat milk",
        &[],
    ));
    assert_eq!(
        desk.client.list_tools().unwrap().len(),
        koru_mcp_server::RUNG1_TOOLS.len()
    );
}

// ------------------------------------------------------ nothing is printed

#[test]
fn no_debug_or_error_text_carries_content_a_title_or_a_handle() {
    const TITLE: &str = "Quarterly-Plan-Title";
    const LINE: &str = "Quarterly-Plan-Body";
    let desk = desk();
    let document = desk.cloud.add_page(TITLE, LINE);
    let mut printed = Vec::new();

    let read = desk
        .client
        .read(
            ReadTool::ReadDocument,
            json!({ "workspace_id": desk.workspace, "document_id": document }),
        )
        .unwrap();
    printed.push(format!("{read:?}"));
    let listed = desk
        .client
        .read(
            ReadTool::ListDocuments,
            json!({ "workspace_id": desk.workspace }),
        )
        .unwrap();
    printed.push(format!("{listed:?}"));
    let found = desk
        .client
        .read(ReadTool::SearchDocuments, json!({ "query": "Quarterly" }))
        .unwrap();
    assert_eq!(found.answer().expose()["hits"].as_array().unwrap().len(), 2);
    printed.push(format!("{found:?}"));
    printed.push(format!("{:?}", desk.client.list_workspaces().unwrap()));

    // A claim: the handle, and the label and purpose in a refusal.
    let taken = desk.client.write(
        WriteTool::ClaimDocument,
        json!({
            "workspace_id": desk.workspace, "document_id": document,
            "purpose": "Quarterly-Plan-Purpose", "agent_label": "Quarterly-Plan-Label",
        }),
    );
    printed.push(format!("{taken:?}"));
    let handle = saved(taken).claim.unwrap().handle;
    let other = desk.cloud.connect("second", Access::ReadAndEdit).client();
    let refused = rewrite(
        &other,
        &desk.workspace,
        &document,
        "Quarterly-Plan-Edit",
        &[],
    );
    printed.push(format!("{refused:?}"));
    printed.push(not_saved(refused).reason.to_string());

    // Each of the other three outcomes of a write that carries content.
    let edit = rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Quarterly-Plan-Edit",
        &[handle.expose()],
    );
    printed.push(format!("{edit:?}"));
    desk.cloud.arm(0, Fate::UnknownButLanded);
    let lost = rewrite(
        &desk.client,
        &desk.workspace,
        &document,
        "Quarterly-Plan-Again",
        &[handle.expose()],
    );
    printed.push(format!("{lost:?}"));
    desk.cloud.arm(1, Fate::NotSaved);
    let partial = create(&desk.client, &desk.workspace, "Quarterly-Plan-New");
    printed.push(format!("{partial:?}"));
    let partial = partially_saved(partial);
    let Recovery::FinishCreate(pending) = &partial.recovery else {
        panic!("a creation is finished with its key");
    };
    printed.push(format!("{pending:?}"));

    for text in &printed {
        assert!(
            !text.contains("Quarterly"),
            "printed a title, content, a label or a purpose: {text}"
        );
        assert!(
            !text.contains(handle.expose()),
            "printed a claim handle: {text}"
        );
    }
    assert!(
        printed.iter().any(|text| text.contains("document_claimed")),
        "a code is still printed"
    );
}
