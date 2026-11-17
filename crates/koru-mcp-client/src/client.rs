// SPDX-License-Identifier: MPL-2.0

//! The client: one method per kind of call.

use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::outcome::{
    Coverage, InvalidCall, NotSaved, PendingCreate, Read, Refusal, Sent, ToolAnswer, WriteOutcome,
    classify_write, refusal_of_tool_error,
};
use crate::protocol::{self, Ids, Reply};
use crate::sealed::Sealed;
use crate::tools::{HostedTool, ReadTool, WriteTool, is_tool_name};
use crate::transport::{AccessTokens, Transport, TransportFailure};

/// The most pages of `tools/list` the client follows. The hosted catalog is
/// fourteen tools; this only stops a server that never ends its list.
const MAX_TOOL_PAGES: usize = 32;

/// A client of one hosted MCP connection.
///
/// It holds the two ports and a request counter, and nothing else: no token,
/// no address, no cache and no state from one call to the next. **It never
/// retries a call.** Each method makes at most one request, except
/// [`Client::list_tools`], which follows the list's pages.
///
/// **Not constructed anywhere in the product.** See the crate's own page.
pub struct Client {
    transport: Arc<dyn Transport>,
    tokens: Arc<dyn AccessTokens>,
    ids: Ids,
}

/// One tool as the server lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedTool {
    pub name: String,
    definition: Sealed<Value>,
}

impl ListedTool {
    /// The tool's whole entry: title, description, input schema, annotations.
    pub fn definition(&self) -> &Sealed<Value> {
        &self.definition
    }
}

/// One workspace the connection may reach, without its name.
///
/// The Profile is the entry's `profile_id`
/// (`contracts/hosted-tools-v1.md` section 5, the `list_workspaces` row). The
/// names stay in the read's sealed answer: a workspace's name is a person's
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListedWorkspace {
    pub id: Uuid,
    pub profile_id: Option<Uuid>,
    pub name_status: NameStatus,
}

/// Whether the server had the workspace's name
/// (`contracts/hosted-tools-v1.md` section 5: `name_status` is `loading`
/// while the names have not arrived, `unavailable` when Cloud cannot serve
/// them, absent otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameStatus {
    Available,
    Loading,
    Unavailable,
}

impl Client {
    pub fn new(transport: Arc<dyn Transport>, tokens: Arc<dyn AccessTokens>) -> Self {
        Self {
            transport,
            tokens,
            ids: Ids::new(),
        }
    }

    /// The server's catalog for this connection, every page of it.
    ///
    /// A Read connection is listed the reads only (HOST.md, "How a call
    /// runs": "`tools/list` follows the token's scope").
    pub fn list_tools(&self) -> Result<Vec<ListedTool>, Refusal> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let reply =
                self.exchange(|id, token| protocol::tools_list(id, cursor.as_deref(), token));
            let result = match reply {
                Exchanged::Reply(Reply::Result(result)) => result,
                Exchanged::Reply(Reply::Refused(refusal)) | Exchanged::Refused(refusal) => {
                    return Err(refusal);
                }
                Exchanged::Reply(Reply::NotUnderstood) => {
                    return Err(Refusal::AnswerNotUnderstood);
                }
                Exchanged::NotSent => return Err(Refusal::NotSent),
                Exchanged::NoAnswer => return Err(Refusal::NoAnswer),
            };
            let listed = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or(Refusal::AnswerNotUnderstood)?;
            for tool in listed {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| is_tool_name(name))
                    .ok_or(Refusal::AnswerNotUnderstood)?;
                tools.push(ListedTool {
                    name: name.to_owned(),
                    definition: Sealed::new(tool.clone()),
                });
            }
            match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(tools),
                Some(Value::String(next)) if cursor.as_deref() != Some(next) => {
                    cursor = Some(next.clone());
                }
                // The same cursor twice, or one that is not text.
                Some(_) => return Err(Refusal::AnswerNotUnderstood),
            }
        }
        Err(Refusal::AnswerNotUnderstood)
    }

    /// The workspaces this connection may reach, by id.
    ///
    /// `list_workspaces` through [`Client::read`], with each entry's id and
    /// Profile read out. An entry without a UUID `id` fails the whole call:
    /// a caller deciding whether a workspace is reachable must not be handed
    /// a shorter list than the server sent.
    pub fn list_workspaces(&self) -> Result<Vec<ListedWorkspace>, Refusal> {
        let read = self.read(ReadTool::ListWorkspaces, Value::Object(Default::default()))?;
        let uuid = |entry: &Value, key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
        };
        read.answer()
            .expose()
            .get("workspaces")
            .and_then(Value::as_array)
            .ok_or(Refusal::AnswerNotUnderstood)?
            .iter()
            .map(|entry| {
                Ok(ListedWorkspace {
                    id: uuid(entry, "id").ok_or(Refusal::AnswerNotUnderstood)?,
                    profile_id: uuid(entry, "profile_id"),
                    name_status: match entry.get("name_status").and_then(Value::as_str) {
                        None => NameStatus::Available,
                        Some("loading") => NameStatus::Loading,
                        // `unavailable`, and any status this client does not
                        // know: the name is not to be relied on.
                        Some(_) => NameStatus::Unavailable,
                    },
                })
            })
            .collect()
    }

    /// Call a tool that changes nothing.
    ///
    /// A read may be repeated: a missing or unreadable answer is an `Err`
    /// like any other, and asking again is safe.
    pub fn read(&self, tool: ReadTool, arguments: Value) -> Result<Read, Refusal> {
        if !arguments.is_object() {
            return Err(Refusal::InvalidCall(InvalidCall::ArgumentsNotAnObject));
        }
        match self.call(tool.name(), &arguments) {
            ToolAnswer::Success(answer) => {
                let coverage = match tool {
                    ReadTool::SearchDocuments => Coverage::of_search(&answer),
                    _ => Coverage::NotApplicable,
                };
                Ok(Read::new(coverage, answer))
            }
            ToolAnswer::Failure(error) => {
                Err(if error.get("code").and_then(Value::as_str).is_some() {
                    refusal_of_tool_error(error)
                } else {
                    Refusal::AnswerNotUnderstood
                })
            }
            ToolAnswer::Refused(refusal) => Err(refusal),
            ToolAnswer::NotSent => Err(Refusal::NotSent),
            ToolAnswer::NoAnswer => Err(Refusal::NoAnswer),
            ToolAnswer::NotUnderstood => Err(Refusal::AnswerNotUnderstood),
        }
    }

    /// Call a tool that can change something, once.
    ///
    /// **`create_document` always goes out with an `idempotency_key`.** When
    /// the arguments have none the client mints a v4 UUID and adds it, so the
    /// key is known before the request leaves and survives an answer that
    /// never arrives. A caller that records its own key before the call
    /// (which is what a caller that must survive its own restart does) passes
    /// it in the arguments and gets the same one back in every outcome.
    pub fn write(&self, tool: WriteTool, mut arguments: Value) -> WriteOutcome {
        let Some(object) = arguments.as_object_mut() else {
            return refused_here(InvalidCall::ArgumentsNotAnObject, None);
        };
        let mut create = None;
        if tool == WriteTool::CreateDocument {
            let key = match object.get("idempotency_key") {
                None | Some(Value::Null) => {
                    let key = Uuid::new_v4();
                    object.insert("idempotency_key".into(), Value::String(key.to_string()));
                    key
                }
                Some(given) => match given.as_str().and_then(|key| Uuid::parse_str(key).ok()) {
                    Some(key) => key,
                    None => return refused_here(InvalidCall::IdempotencyKeyNotAUuid, None),
                },
            };
            create = Some(PendingCreate::new(key, arguments.clone()));
        }
        let answer = self.call(tool.name(), &arguments);
        classify_write(
            Sent {
                tool: Some(tool),
                create,
            },
            answer,
        )
    }

    /// Send a creation again, exactly as it was, under the same key.
    ///
    /// The one repeat this client offers. It is safe whatever became of the
    /// first attempt: the server replays a creation it already made and
    /// finishes one it left unlisted (`contracts/hosted-tools-v1.md` section
    /// 2: "`create_document` with the same `idempotency_key` is always safe
    /// to retry"). The local contract keeps a key for seven days
    /// (`crates/koru-mcp-server/MCP.md`, "Creating a complete document");
    /// how long the hosted server keeps one is not in either document.
    ///
    /// It can still answer `idempotency_key_retired`, when the document the
    /// key made has since been deleted, which is a refusal like any other.
    pub fn finish_create(&self, pending: &PendingCreate) -> WriteOutcome {
        let answer = self.call(WriteTool::CreateDocument.name(), pending.arguments());
        classify_write(
            Sent {
                tool: Some(WriteTool::CreateDocument),
                create: Some(pending.clone()),
            },
            answer,
        )
    }

    /// Call a tool by a name the hosted catalog does not have.
    ///
    /// The server refuses any such name `tool_not_available` "with no backend
    /// work" (`contracts/hosted-tools-v1.md` section 3), and this exists so a
    /// caller that forwards names can pass one through and hand back that
    /// refusal. The client cannot know what an unlisted tool does, so it is
    /// treated as a write: any answer that is not a plain refusal or a plain
    /// success is unknown.
    pub fn call_unlisted(&self, name: &str, arguments: Value) -> WriteOutcome {
        if !is_tool_name(name) || HostedTool::from_name(name).is_some() {
            return refused_here(InvalidCall::ToolName, None);
        }
        if !arguments.is_object() {
            return refused_here(InvalidCall::ArgumentsNotAnObject, None);
        }
        let answer = self.call(name, &arguments);
        classify_write(
            Sent {
                tool: None,
                create: None,
            },
            answer,
        )
    }

    /// One `tools/call`.
    fn call(&self, name: &str, arguments: &Value) -> ToolAnswer {
        match self.exchange(|id, token| protocol::tools_call(id, name, arguments, token)) {
            Exchanged::Reply(reply) => protocol::tool_answer(reply),
            Exchanged::Refused(refusal) => ToolAnswer::Refused(refusal),
            Exchanged::NotSent => ToolAnswer::NotSent,
            Exchanged::NoAnswer => ToolAnswer::NoAnswer,
        }
    }

    /// One request and its response.
    fn exchange(
        &self,
        build: impl FnOnce(u64, crate::AccessToken) -> crate::PostRequest,
    ) -> Exchanged {
        let token = match self.tokens.access_token() {
            Ok(token) => token,
            // Nothing was sent, whatever reason the credential's owner gave.
            // One that is not a proof on its own is reported as what it is
            // here: a request that never left.
            Err(refusal) if refusal.proves_nothing_ran() => return Exchanged::Refused(refusal),
            Err(_) => return Exchanged::NotSent,
        };
        let id = self.ids.next();
        let request = build(id, token);
        match self.transport.post(&request) {
            Ok(response) => Exchanged::Reply(protocol::read_reply(id, &response)),
            Err(TransportFailure::NotSent) => Exchanged::NotSent,
            Err(TransportFailure::Unconfirmed) => Exchanged::NoAnswer,
        }
    }
}

enum Exchanged {
    Reply(Reply),
    /// The credential's owner had no token to give.
    Refused(Refusal),
    NotSent,
    NoAnswer,
}

/// A write the client refused before sending anything.
fn refused_here(why: InvalidCall, idempotency_key: Option<Uuid>) -> WriteOutcome {
    WriteOutcome::NotSaved(NotSaved {
        reason: Refusal::InvalidCall(why),
        idempotency_key,
    })
}

#[cfg(test)]
mod tests;
