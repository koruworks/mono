// SPDX-License-Identifier: MPL-2.0
//! Scripted Codex protocol peer, linked only into the explicit fixture root.
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub fn trace(label: &str) {
    if let Some(path) = std::env::var_os("BOTS_ENGINE_FIXTURE_TRACE") {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        writeln!(file, "{label}").unwrap();
    }
}

fn emit(value: Value) {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value).unwrap();
    output.write_all(b"\n").unwrap();
    output.flush().unwrap();
}
fn call(name: &str, arguments: Value) -> Value {
    trace(name);
    let endpoint = std::env::args()
        .find_map(|arg| {
            arg.strip_prefix("mcp_servers.koru_run.url=")
                .map(|value| value.trim_matches('\'').to_owned())
        })
        .unwrap();
    assert!(endpoint.starts_with("http://127.0.0.1:"));
    let token = std::env::var("KORU_RUN_MCP_TOKEN").unwrap();
    let response: Value = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(20)).build().unwrap()
        .post(endpoint).bearer_auth(token).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .send().unwrap().error_for_status().unwrap().json().unwrap();
    trace(&format!(
        "tool result error={} status={} code={}",
        response["result"]["isError"],
        response["result"]["structuredContent"]["status"],
        response["result"]["structuredContent"]["code"]
    ));
    assert_eq!(
        response["result"]["isError"], false,
        "synthetic tool refused: {}",
        response["result"]["structuredContent"]["code"]
    );
    response["result"]["structuredContent"].clone()
}
pub fn run() {
    for line in std::io::stdin().lock().lines() {
        let packet: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = &packet["id"];
        trace(packet["method"].as_str().unwrap_or("response"));
        match packet["method"].as_str().unwrap_or_default() {
            "initialize" => emit(
                json!({"id":id,"result":{"userAgent":"koru_terminal/0.166.0 (synthetic)","codexHome":"synthetic","platformFamily":"windows","platformOs":"windows"}}),
            ),
            "thread/start" => emit(json!({"id":id,"result":{"thread":{"id":"synthetic-thread"}}})),
            "thread/resume" => {
                emit(json!({"id":id,"error":{"code":-32000,"message":"synthetic rebuild"}}))
            }
            "model/list" => emit(
                json!({"id":id,"result":{"data":[{"id":"fixture-model","model":"fixture-model","displayName":"Synthetic","description":"Synthetic","isDefault":true,"hidden":false,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[],"inputModalities":["text","image"]}],"nextCursor":null}}),
            ),
            "turn/start" => {
                emit(
                    json!({"id":id,"result":{"turn":{"id":"synthetic-turn","status":"inProgress","items":[]}}}),
                );
                let input = packet["params"]["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "text")
                    .unwrap()["text"]
                    .as_str()
                    .unwrap();
                let input: Value = serde_json::from_str(input).unwrap();
                let work = input["delivery"]["work"].as_array().unwrap();
                if work
                    .iter()
                    .any(|message| message["content"]["body"] == "HOLD")
                {
                    trace("holding-turn");
                    continue;
                }
                if let Some(files) = input["files"].as_array() {
                    for file in files {
                        assert_eq!(
                            std::fs::read(file["path"].as_str().unwrap()).unwrap(),
                            b"SYNTHETIC FILE"
                        );
                    }
                    trace("attachment-bytes-verified");
                }
                let workspace = input["delivery"]["work"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["content"]["references"].as_array().unwrap())
                    .find(|r| r["kind"] == "fixture_workspace")
                    .unwrap()["workspace"]
                    .clone();
                let arguments = json!({"operation_id":work[0]["id"],"workspace_id":workspace,"title":"Synthetic engine result","initial_operations":[{"op":"insert","node_id":"body","parent_id":"root","after_node_id":null,"kind":"paragraph","text":"SYNTHETIC ORIGINAL"}]});
                let created = call("create_document", arguments.clone());
                assert_eq!(created["status"], "applied");
                let replay = call("create_document", arguments);
                assert_eq!(created["reference"], replay["reference"]);
                let read = call(
                    "read_document",
                    json!({"workspace_id":workspace,"document_id":created["reference"]["document"]}),
                );
                assert!(
                    read["nodes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|node| node["content"]["text"] == "SYNTHETIC ORIGINAL")
                );
                if work
                    .iter()
                    .any(|message| message["content"]["body"] == "RECOVER")
                {
                    let root = std::path::PathBuf::from(
                        std::env::var_os("BOTS_ENGINE_FIXTURE_ROOT").unwrap(),
                    );
                    if !root.join("resume.allowed").exists() {
                        std::fs::write(root.join("recovery.ready"), b"applied").unwrap();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(30);
                        while !root.join("resume.allowed").exists() {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "fixture restart deadline"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                    }
                }
                emit(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":"synthetic-thread","turnId":"synthetic-turn","itemId":"synthetic-message","delta":"SYNTHETIC COMPLETE"}}),
                );
                emit(
                    json!({"method":"turn/completed","params":{"threadId":"synthetic-thread","turn":{"id":"synthetic-turn","status":"completed","items":[],"error":null}}}),
                );
            }
            "turn/interrupt" => emit(json!({"id":id,"result":{}})),
            _ if !id.is_null() => emit(json!({"id":id,"result":{}})),
            _ => (),
        }
    }
}
