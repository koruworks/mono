// SPDX-License-Identifier: MPL-2.0

//! Process-local proof of a previously issued snapshot/position, never a read
//! capability. Every continuation still requires current canonical authority.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_ai_core::bots::*;
use koru_ai_core::{DocumentId, ProfileId, WorkspaceId};
use koru_delegate_core::{AttemptGeneration, DocumentVersionId, RunId};
use serde::{Deserialize, Serialize};
use std::time::Instant;

const DOMAIN: &[u8] = b"koru-bot-memory-cursor/1\0";
const TTL_MS: u64 = 15 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", deny_unknown_fields)]
pub(super) enum Subject {
    #[serde(rename = "o")]
    Owner {},
    #[serde(rename = "r")]
    Run {
        #[serde(rename = "r")]
        run: RunId,
        #[serde(rename = "g")]
        generation: AttemptGeneration,
        #[serde(rename = "s")]
        submission: BotRequestId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub owner: BotOwner,
    pub bot: BotId,
    pub profile: ProfileId,
    pub page: BotPage,
    pub creation_request: BotRequestId,
    pub view: MemoryView,
    pub subject: Subject,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", deny_unknown_fields)]
enum Position {
    #[serde(rename = "l")]
    List { a: u32, l: u16 },
    #[serde(rename = "d")]
    Detail { f: BotMemoryNodeId, o: u32, p: u32 },
}
impl Position {
    fn from_read(position: &MemoryReadPosition) -> Self {
        match *position {
            MemoryReadPosition::List { after, limit } => Self::List { a: after, l: limit },
            MemoryReadPosition::Detail {
                fact_id,
                node_ordinal,
                start_utf8,
            } => Self::Detail {
                f: fact_id,
                o: node_ordinal,
                p: start_utf8,
            },
        }
    }
    fn to_read(&self) -> MemoryReadPosition {
        match *self {
            Self::List { a, l } => MemoryReadPosition::List { after: a, limit: l },
            Self::Detail { f, o, p } => MemoryReadPosition::Detail {
                fact_id: f,
                node_ordinal: o,
                start_utf8: p,
            },
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::List { a, l } => *a <= 50_000 && (1..=BOT_MEMORY_LIST_LIMIT).contains(l),
            Self::Detail { o, p, .. } => *o <= 50_000 && *p <= 8 * 1024 * 1024,
        }
    }
}

/// Short closed keys keep the full UUID subject plus binary MAC within the
/// single issued-token ceiling. JSON is canonicalized by exact reserialization.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    v: u8,
    u: UserScopeId,
    d: DeviceId,
    b: BotId,
    p: ProfileId,
    w: WorkspaceId,
    m: DocumentId,
    c: BotRequestId,
    h: DocumentVersionId,
    s: Subject,
    t: MemoryView,
    i: u64,
    x: u64,
    n: Position,
}
impl Claims {
    fn scope(&self) -> Scope {
        Scope {
            owner: BotOwner {
                user_scope: self.u,
                device: self.d,
            },
            bot: self.b,
            profile: self.p,
            page: BotPage {
                workspace: self.w,
                document: self.m,
            },
            creation_request: self.c,
            view: self.t,
            subject: self.s.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CursorError {
    Invalid,
    Expired,
}

pub(super) struct Continuation {
    pub version: DocumentVersionId,
    pub position: MemoryReadPosition,
    /// Preserve this pair on every page; pagination never extends the lifetime.
    pub lifetime: (u64, u64),
}

pub(super) struct CursorKey {
    owner: BotOwner,
    key: [u8; 32],
    started: Instant,
}
impl CursorKey {
    pub fn new(owner: BotOwner) -> Self {
        let mut key = [0; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            owner,
            key,
            started: Instant::now(),
        }
    }
    fn elapsed(&self) -> Result<u64, CursorError> {
        u64::try_from(self.started.elapsed().as_millis()).map_err(|_| CursorError::Expired)
    }
    pub fn issue(
        &self,
        scope: &Scope,
        version: DocumentVersionId,
        position: &MemoryReadPosition,
        lifetime: Option<(u64, u64)>,
    ) -> Result<String, CursorError> {
        self.issue_at(scope, version, position, lifetime, self.elapsed()?)
    }
    fn issue_at(
        &self,
        scope: &Scope,
        version: DocumentVersionId,
        position: &MemoryReadPosition,
        lifetime: Option<(u64, u64)>,
        now: u64,
    ) -> Result<String, CursorError> {
        if scope.owner != self.owner {
            return Err(CursorError::Invalid);
        }
        let (issued, expires) = match lifetime {
            Some(pair) => pair,
            None => (now, now.checked_add(TTL_MS).ok_or(CursorError::Expired)?),
        };
        let claims = Claims {
            v: 1,
            u: scope.owner.user_scope,
            d: scope.owner.device,
            b: scope.bot,
            p: scope.profile,
            w: scope.page.workspace,
            m: scope.page.document,
            c: scope.creation_request,
            h: version,
            s: scope.subject.clone(),
            t: scope.view,
            i: issued,
            x: expires,
            n: Position::from_read(position),
        };
        validate(&claims, now)?;
        let mut bytes = serde_json::to_vec(&claims).map_err(|_| CursorError::Invalid)?;
        let tag = self.mac(&bytes);
        bytes.extend_from_slice(tag.as_bytes());
        let token = URL_SAFE_NO_PAD.encode(bytes);
        if token.len() > BOT_MEMORY_ISSUED_CURSOR_BYTES {
            return Err(CursorError::Invalid);
        }
        Ok(token)
    }
    fn mac(&self, bytes: &[u8]) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.key);
        hasher.update(DOMAIN);
        hasher.update(bytes);
        hasher.finalize()
    }
    pub fn open(
        &self,
        token: &str,
        scope: &Scope,
        operation: &MemoryReadOperation,
    ) -> Result<Continuation, CursorError> {
        operation
            .validate_bounds()
            .map_err(|_| CursorError::Invalid)?;
        let supplied = match operation {
            MemoryReadOperation::List { cursor, .. }
            | MemoryReadOperation::Detail { cursor, .. } => cursor,
        };
        if supplied.as_deref() != Some(token) {
            return Err(CursorError::Invalid);
        }
        let continuation = self.open_at(token, scope, self.elapsed()?)?;
        match (&continuation.position, operation) {
            (
                MemoryReadPosition::List { limit, .. },
                MemoryReadOperation::List {
                    view,
                    limit: requested,
                    ..
                },
            ) if *view == scope.view && limit == requested => {}
            (
                MemoryReadPosition::Detail { fact_id, .. },
                MemoryReadOperation::Detail {
                    view,
                    fact_id: requested,
                    ..
                },
            ) if *view == scope.view && fact_id == requested => {}
            _ => return Err(CursorError::Invalid),
        }
        Ok(continuation)
    }
    fn open_at(&self, token: &str, scope: &Scope, now: u64) -> Result<Continuation, CursorError> {
        if scope.owner != self.owner
            || token.is_empty()
            || token.len() > BOT_MEMORY_CURSOR_BYTES
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(CursorError::Invalid);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| CursorError::Invalid)?;
        if URL_SAFE_NO_PAD.encode(&bytes) != token || bytes.len() <= 32 {
            return Err(CursorError::Invalid);
        }
        let (payload, tag) = bytes.split_at(bytes.len() - 32);
        let tag = blake3::Hash::from_bytes(tag.try_into().map_err(|_| CursorError::Invalid)?);
        // blake3::Hash equality uses a constant-time comparison.
        if self.mac(payload) != tag {
            return Err(CursorError::Invalid);
        }
        let claims: Claims = serde_json::from_slice(payload).map_err(|_| CursorError::Invalid)?;
        if serde_json::to_vec(&claims).map_err(|_| CursorError::Invalid)? != payload
            || claims.scope() != *scope
        {
            return Err(CursorError::Invalid);
        }
        validate(&claims, now)?;
        Ok(Continuation {
            version: claims.h,
            position: claims.n.to_read(),
            lifetime: (claims.i, claims.x),
        })
    }
}

fn validate(claims: &Claims, now: u64) -> Result<(), CursorError> {
    if claims.v != 1
        || !claims.n.valid()
        || matches!(claims.s, Subject::Run { generation, .. } if generation.get() == 0)
        || matches!(claims.s, Subject::Owner {}) && claims.t != MemoryView::Current
        || claims.i > now
        || claims.x <= claims.i
        || claims.x - claims.i > TTL_MS
    {
        return Err(CursorError::Invalid);
    }
    if now >= claims.x {
        return Err(CursorError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
