// SPDX-License-Identifier: MPL-2.0

//! Document claims, scripted by hand from `contracts/hosted-tools-v1.md`
//! section 4.
//!
//! The shared crate publishes the two claim tools and admits them; only the
//! hosted server implements them (`koru_mcp_server::hosted`), and its source
//! is not in this repository. **So every line here is the contract's text
//! turned into code, and none of it has been compared with the real thing.**
//! Each rule names the sentence it came from.

use koru_mcp_server::ToolError;
use koru_mcp_server::hosted::{MAX_CLAIM_ID_BYTES, MAX_CLAIM_IDS};
use serde_json::{Value, json};

/// "`claim_limit_reached`: the connection already holds 16 live claims".
const MAX_LIVE_CLAIMS: usize = 16;

struct Claim {
    handle: String,
    document: String,
    connection: String,
    purpose: Option<String>,
    agent_label: Option<String>,
    /// Unix milliseconds.
    expires_at: i64,
}

impl Claim {
    fn live(&self, now: i64) -> bool {
        self.expires_at > now
    }

    /// "`holder` is `{ connection_name, client, verified, agent_label }`".
    /// The fake's connections are all unverified clients of one name.
    fn holder(&self) -> Value {
        json!({
            "connection_name": self.connection,
            "client": "fake client",
            "verified": false,
            "agent_label": self.agent_label,
        })
    }
}

#[derive(Default)]
pub(super) struct Claims {
    claims: Vec<Claim>,
    minted: u64,
}

fn refusal(code: &str, message: &str) -> ToolError {
    ToolError {
        code: code.into(),
        message: message.into(),
        details: None,
    }
}

fn text<'a>(arguments: &'a Value, key: &str, most: usize) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() && text.chars().count() <= most => {
            Ok(Some(text))
        }
        Some(_) => Err(refusal(
            "invalid_request",
            &format!("{key} must be 1 to {most} characters"),
        )),
    }
}

impl Claims {
    /// `claim_document`: take a claim, or renew one by its handle.
    pub(super) fn take(
        &mut self,
        now: i64,
        connection: &str,
        arguments: &Value,
    ) -> Result<Value, ToolError> {
        let document = arguments
            .get("document_id")
            .and_then(Value::as_str)
            .ok_or_else(|| refusal("invalid_request", "document_id is required"))?
            .to_owned();
        // "`purpose`: string, 1–120 characters"; "`agent_label`: string, 1–64
        // characters"; "`duration_seconds`: integer, 60–900, default 300".
        let purpose = text(arguments, "purpose", 120)?.map(str::to_owned);
        let agent_label = text(arguments, "agent_label", 64)?.map(str::to_owned);
        let renewing = text(arguments, "claim_id", MAX_CLAIM_ID_BYTES)?;
        let duration = match arguments.get("duration_seconds") {
            None | Some(Value::Null) => 300,
            Some(seconds) => seconds
                .as_i64()
                .filter(|seconds| (60..=900).contains(seconds))
                .ok_or_else(|| refusal("invalid_request", "duration_seconds must be 60 to 900"))?,
        };
        let expires_at = now + duration * 1_000;

        if let Some(handle) = renewing {
            // "`claim_not_found`: renewing a handle the server does not
            // hold"; "`claim_expired`: renewing a handle whose claim has
            // expired". Renewing "keeps the handle".
            let Some(index) = self.claims.iter().position(|claim| {
                claim.handle == handle
                    && claim.connection == connection
                    && claim.document == document
            }) else {
                return Err(refusal(
                    "claim_not_found",
                    "No claim is held under that handle.",
                ));
            };
            if !self.claims[index].live(now) {
                self.claims.remove(index);
                return Err(refusal("claim_expired", "That claim has expired."));
            }
            let claim = &mut self.claims[index];
            claim.expires_at = expires_at;
            if purpose.is_some() {
                claim.purpose = purpose;
            }
            if agent_label.is_some() {
                claim.agent_label = agent_label;
            }
            return Ok(json!({
                "claim_id": claim.handle,
                "expires_at": claim.expires_at,
                "holder": claim.holder(),
            }));
        }

        // "`document_claimed`: ... `claim_document` on a document someone
        // else holds". A second claim by the same connection is under
        // another handle, so it is someone else's too.
        if let Some(held) = self.live_on(now, &document) {
            return Err(self.claimed(now, held));
        }
        self.claims.retain(|claim| claim.document != document);
        let held_now = self
            .claims
            .iter()
            .filter(|claim| claim.connection == connection && claim.live(now))
            .count();
        if held_now >= MAX_LIVE_CLAIMS {
            return Err(refusal(
                "claim_limit_reached",
                "This connection already holds 16 live claims.",
            ));
        }
        // "`claim_id` is an opaque, server-minted handle."
        self.minted += 1;
        let claim = Claim {
            handle: format!(
                "fake-claim-{}-{}",
                self.minted,
                uuid::Uuid::new_v4().simple()
            ),
            document,
            connection: connection.to_owned(),
            purpose,
            agent_label,
            expires_at,
        };
        let answer = json!({
            "claim_id": claim.handle,
            "expires_at": claim.expires_at,
            "holder": claim.holder(),
        });
        self.claims.push(claim);
        Ok(answer)
    }

    /// `release_document`: "Idempotent: releasing a claim that is already
    /// released or expired answers the same." A handle that is another
    /// connection's releases nothing and answers the same too.
    pub(super) fn release(
        &mut self,
        connection: &str,
        arguments: &Value,
    ) -> Result<Value, ToolError> {
        let handle = arguments
            .get("claim_id")
            .and_then(Value::as_str)
            .ok_or_else(|| refusal("invalid_request", "claim_id is required"))?;
        self.claims
            .retain(|claim| !(claim.handle == handle && claim.connection == connection));
        Ok(json!({ "claim_id": handle }))
    }

    /// A revoked connection's claims go with it.
    pub(super) fn release_all(&mut self, connection: &str) {
        self.claims.retain(|claim| claim.connection != connection);
    }

    /// The check before a mutating call's first effect: "every target under a
    /// live claim needs that claim's handle". The handle only counts from the
    /// connection that holds the claim: a change is refused "on other
    /// connections, and on the same connection without the handle".
    pub(super) fn check(
        &self,
        now: i64,
        connection: &str,
        document: &str,
        claim_ids: &[String],
    ) -> Result<(), ToolError> {
        let Some(held) = self.live_on(now, document) else {
            return Ok(());
        };
        let presented =
            held.connection == connection && claim_ids.iter().any(|handle| *handle == held.handle);
        if presented {
            Ok(())
        } else {
            Err(self.claimed(now, held))
        }
    }

    /// "`claim: { holder, purpose, expires_at }` for a document under a live
    /// claim, and omit it otherwise."
    pub(super) fn shown(&self, now: i64, document: &str) -> Option<Value> {
        self.live_on(now, document).map(|claim| {
            json!({
                "holder": claim.holder(),
                "purpose": claim.purpose,
                "expires_at": claim.expires_at,
            })
        })
    }

    fn live_on(&self, now: i64, document: &str) -> Option<&Claim> {
        self.claims
            .iter()
            .find(|claim| claim.document == document && claim.live(now))
    }

    /// "`{ document_id, holder, purpose, expires_at, retry_after_seconds }`",
    /// and a message that "tells the assistant to wait until `expires_at` or
    /// retry later, and not to work around the claim".
    fn claimed(&self, now: i64, held: &Claim) -> ToolError {
        ToolError {
            code: "document_claimed".into(),
            message: "Another agent holds a claim on this document. Wait until expires_at or retry later; do not work around the claim.".into(),
            details: Some(json!({
                "document_id": held.document,
                "holder": held.holder(),
                "purpose": held.purpose,
                "expires_at": held.expires_at,
                "retry_after_seconds": ((held.expires_at - now).max(0) + 999) / 1_000,
            })),
        }
    }
}

/// The handles a call presented: "an optional `claim_ids: string[]`, at most
/// 2 items". Removed from the arguments, because the shared tools' own
/// request types do not know the field.
pub(super) fn take_claim_ids(arguments: &mut Value) -> Result<Vec<String>, ToolError> {
    let Some(given) = arguments
        .as_object_mut()
        .and_then(|object| object.remove("claim_ids"))
    else {
        return Ok(Vec::new());
    };
    let handles = given
        .as_array()
        .filter(|handles| handles.len() <= MAX_CLAIM_IDS)
        .ok_or_else(|| {
            refusal(
                "invalid_request",
                "claim_ids is a list of at most 2 handles",
            )
        })?;
    handles
        .iter()
        .map(|handle| {
            handle
                .as_str()
                .filter(|handle| !handle.is_empty() && handle.len() <= MAX_CLAIM_ID_BYTES)
                .map(str::to_owned)
                .ok_or_else(|| refusal("invalid_request", "a claim handle is 1 to 128 bytes"))
        })
        .collect()
}
