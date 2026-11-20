// SPDX-License-Identifier: MPL-2.0

//! Shared list/search/read semantics for chats exposed as tools.

use serde::{Deserialize, Serialize};

use crate::{
    AiError, AiErrorClass, AiPersistence, AiSearchQuery, ChatAccessLens, ChatId, ChatRole,
    MessageId, MessageState, ProfileId, Timestamp,
};

const DEFAULT_LIST_PAGE_SIZE: u16 = 50;
const DEFAULT_SEARCH_PAGE_SIZE: u16 = 20;
const DEFAULT_READ_PAGE_SIZE: u16 = 20;
const MAX_LIST_PAGE_SIZE: u16 = 200;
const MAX_SEARCH_PAGE_SIZE: u16 = 200;
const MAX_READ_PAGE_SIZE: u16 = 50;
const MAX_MESSAGE_MARKDOWN_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ListChatsToolRequest {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub page_size: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ListChatsToolResponse {
    pub chats: Vec<ChatToolSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatToolSummary {
    pub chat_id: ChatId,
    pub title: String,
    pub message_count: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SearchChatsToolRequest {
    pub query: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub page_size: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchChatsToolResponse {
    pub hits: Vec<ChatToolSearchHit>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatToolSearchHit {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub chat_title: String,
    pub snippet: String,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReadChatToolRequest {
    pub chat_id: ChatId,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub page_size: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReadChatToolResponse {
    pub chat_id: ChatId,
    pub title: String,
    pub total_messages: u32,
    pub messages: Vec<ChatToolMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatToolMessage {
    pub message_id: MessageId,
    pub ordinal: u32,
    pub role: ChatRole,
    pub state: MessageState,
    pub markdown: String,
    pub truncated: bool,
    pub created_at: Timestamp,
}

/// The chats a tool may list. **A chat of a profile in `removed`** — one
/// removed from this computer ([`crate::StoreContextPort::removed_profiles`])
/// — is left out before the page is cut, so a page is never short of what
/// its cursor promises.
pub fn list_chats(
    persistence: &dyn AiPersistence,
    lens: ChatAccessLens,
    request: ListChatsToolRequest,
    removed: &[ProfileId],
) -> Result<ListChatsToolResponse, AiError> {
    let offset = offset_cursor(request.cursor.as_deref())?;
    let page_size = usize::from(
        request
            .page_size
            .unwrap_or(DEFAULT_LIST_PAGE_SIZE)
            .clamp(1, MAX_LIST_PAGE_SIZE),
    );
    let scope = profile_scope(persistence, lens)?;
    let visible = persistence
        .chat_overviews()?
        .into_iter()
        .filter(|chat| lens_allows(lens, chat.chat_id, chat.private))
        .filter(|chat| scope.is_none_or(|profile| chat.profile_id == profile))
        .filter(|chat| {
            chat.profile_id
                .is_none_or(|profile| !removed.contains(&profile))
        })
        .collect::<Vec<_>>();
    let chats = visible
        .iter()
        .skip(offset)
        .take(page_size)
        .map(|chat| ChatToolSummary {
            chat_id: chat.chat_id,
            title: chat.title.clone(),
            message_count: chat.message_count,
            created_at: chat.created_at,
            updated_at: chat.updated_at,
        })
        .collect::<Vec<_>>();
    let consumed = offset.saturating_add(chats.len());
    Ok(ListChatsToolResponse {
        chats,
        next_cursor: (consumed < visible.len()).then(|| consumed.to_string()),
    })
}

/// The responses a tool may find, leaving out **a chat of a profile in
/// `removed`** in the search itself, as [`list_chats`] does.
pub fn search_chats(
    persistence: &dyn AiPersistence,
    lens: ChatAccessLens,
    request: SearchChatsToolRequest,
    removed: &[ProfileId],
) -> Result<SearchChatsToolResponse, AiError> {
    let page = persistence.search(&AiSearchQuery {
        text: request.query,
        limit: request
            .page_size
            .unwrap_or(DEFAULT_SEARCH_PAGE_SIZE)
            .clamp(1, MAX_SEARCH_PAGE_SIZE),
        cursor: request.cursor,
        visibility: lens,
        // A calling chat's own profile is applied by the search itself.
        profile: None,
        removed_profiles: removed.to_vec(),
    })?;
    Ok(SearchChatsToolResponse {
        hits: page
            .hits
            .into_iter()
            .map(|hit| ChatToolSearchHit {
                chat_id: hit.chat_id,
                message_id: hit.message_id,
                chat_title: hit.chat_title,
                snippet: hit.snippet,
                score: hit.score,
            })
            .collect(),
        next_cursor: page.next_cursor,
    })
}

pub fn read_chat(
    persistence: &dyn AiPersistence,
    lens: ChatAccessLens,
    request: ReadChatToolRequest,
) -> Result<ReadChatToolResponse, AiError> {
    let scope = profile_scope(persistence, lens)?;
    let chat = persistence
        .chat(request.chat_id)?
        .filter(|chat| lens_allows(lens, chat.id, chat.private))
        .filter(|chat| scope.is_none_or(|profile| chat.profile_id == profile))
        .ok_or_else(chat_not_found)?;
    let before = ordinal_cursor(request.cursor.as_deref())?;
    let page_size = usize::from(
        request
            .page_size
            .unwrap_or(DEFAULT_READ_PAGE_SIZE)
            .clamp(1, MAX_READ_PAGE_SIZE),
    );
    let terminal = persistence
        .messages(chat.id)?
        .into_iter()
        .filter(|message| message.state.is_terminal())
        .collect::<Vec<_>>();
    let total_messages = u32::try_from(terminal.len()).unwrap_or(u32::MAX);
    let eligible = terminal
        .iter()
        .filter(|message| before.is_none_or(|before| message.ordinal < before))
        .collect::<Vec<_>>();
    let start = eligible.len().saturating_sub(page_size);
    let messages = eligible[start..]
        .iter()
        .map(|message| {
            let (markdown, truncated) = truncate_markdown(&message.markdown);
            ChatToolMessage {
                message_id: message.id,
                ordinal: message.ordinal,
                role: message.role,
                state: message.state,
                markdown,
                truncated,
                created_at: message.created_at,
            }
        })
        .collect::<Vec<_>>();
    let next_cursor = (start > 0)
        .then(|| messages.first().map(|message| message.ordinal.to_string()))
        .flatten();
    Ok(ReadChatToolResponse {
        chat_id: chat.id,
        title: chat.title,
        total_messages,
        messages,
        next_cursor,
    })
}

/// **A chat's tools see only its own profile's chats** (AI schema 13): the
/// profile the calling chat is stored with, which no request can name.
/// `None` where nothing narrows the list — the person's own surface, and an
/// outside MCP client, which is not a chat.
fn profile_scope(
    persistence: &dyn AiPersistence,
    lens: ChatAccessLens,
) -> Result<Option<Option<ProfileId>>, AiError> {
    match lens {
        ChatAccessLens::ToolSurface {
            calling_chat: Some(calling),
        } => Ok(Some(
            persistence.chat(calling)?.and_then(|chat| chat.profile_id),
        )),
        _ => Ok(None),
    }
}

fn lens_allows(lens: ChatAccessLens, chat: ChatId, private: bool) -> bool {
    !private
        || match lens {
            ChatAccessLens::UserSurface => true,
            ChatAccessLens::ToolSurface { calling_chat } => calling_chat == Some(chat),
        }
}

fn offset_cursor(cursor: Option<&str>) -> Result<usize, AiError> {
    cursor
        .map(str::parse)
        .transpose()
        .map(|offset| offset.unwrap_or_default())
        .map_err(|_| invalid_cursor())
}

fn ordinal_cursor(cursor: Option<&str>) -> Result<Option<u32>, AiError> {
    cursor
        .map(str::parse)
        .transpose()
        .map_err(|_| invalid_cursor())
}

fn truncate_markdown(markdown: &str) -> (String, bool) {
    if markdown.len() <= MAX_MESSAGE_MARKDOWN_BYTES {
        return (markdown.to_owned(), false);
    }
    let mut end = MAX_MESSAGE_MARKDOWN_BYTES;
    while !markdown.is_char_boundary(end) {
        end -= 1;
    }
    (markdown[..end].to_owned(), true)
}

fn invalid_cursor() -> AiError {
    AiError::new(
        AiErrorClass::Configuration,
        "The chat tool cursor is invalid.",
        false,
    )
}

fn chat_not_found() -> AiError {
    AiError::new(AiErrorClass::Storage, "Chat not found.", false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let markdown = format!("{}é", "a".repeat(MAX_MESSAGE_MARKDOWN_BYTES - 1));
        let (truncated, was_truncated) = truncate_markdown(&markdown);
        assert!(was_truncated);
        assert_eq!(truncated.len(), MAX_MESSAGE_MARKDOWN_BYTES - 1);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn invalid_cursors_are_rejected() {
        assert!(offset_cursor(Some("not-a-number")).is_err());
        assert!(ordinal_cursor(Some("-1")).is_err());
    }
}
