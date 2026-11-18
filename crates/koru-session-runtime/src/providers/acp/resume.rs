// SPDX-License-Identifier: MPL-2.0

//! Native history is evidence, never a second stream of live activity.
use super::*;

const QUALIFIED_WRAPPER: &str = "0.76.0";
const MAX_INPUT_HASHES: usize = 1024;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    wrapper: String,
    session: String,
}

impl AcpControl {
    pub(super) fn qualified_resume_route(&self) -> bool {
        // P0.2 qualified populated history and replacement MCP credentials
        // only on this exact route. A newer executable may start, but must
        // rebuild until its resume behavior has been qualified separately.
        self.config.route.is_claude()
            && self.config.executable_version.as_str() == QUALIFIED_WRAPPER
    }

    pub(super) fn saved_resume_token(&self) -> Option<ProviderResumeToken> {
        if !self.native_resume || self.capabilities.is_none() {
            return None;
        }
        serde_json::to_string(&Saved {
            wrapper: QUALIFIED_WRAPPER.into(),
            session: self.native_session.clone()?,
        })
        .ok()?
        .parse()
        .ok()
    }

    pub(super) fn load_session(
        &mut self,
        token: &ProviderResumeToken,
        mut params: Value,
    ) -> Result<(Value, String), ProviderFailure> {
        let saved: Saved =
            serde_json::from_str(token.as_str()).map_err(|_| handshake_rejected())?;
        if saved.wrapper != QUALIFIED_WRAPPER || !valid_native_id(&saved.session) {
            return Err(handshake_rejected());
        }
        params["sessionId"] = json!(saved.session);
        self.loading_session = Some(saved.session.clone());
        self.replayed_inputs.clear();
        // The freshly built params contain this incarnation's endpoint and
        // credential. Neither lives in the saved token.
        let result = self.call("session/load", params);
        self.loading_session = None;
        let session = match result.and_then(|value| {
            decode::<acp::LoadSessionResponse>(&value).map_err(|_| handshake_rejected())?;
            Ok(value)
        }) {
            Ok(session) => session,
            Err(error) => {
                self.replayed_inputs.clear();
                // A failed load may have partly initialized the native
                // session. A caller must retire this process before rebuild.
                self.unusable = Some(error.clone());
                return Err(error);
            }
        };
        Ok((session, saved.session))
    }

    pub(super) fn replay_update(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        if params["sessionId"].as_str() != self.loading_session.as_deref() {
            return Ok(());
        }
        let update = &params["update"];
        if update["sessionUpdate"] == "user_message_chunk" && update["content"]["type"] == "text" {
            decode::<acp::SessionNotification>(params)?;
            if let Some(text) = update["content"]["text"].as_str() {
                let hash = *blake3::hash(text.as_bytes()).as_bytes();
                if !self.replayed_inputs.contains(&hash) {
                    if self.replayed_inputs.len() == MAX_INPUT_HASHES {
                        self.replayed_inputs.remove(0);
                    }
                    self.replayed_inputs.push(hash);
                }
            }
        }
        // Includes old tool calls, replies, notices, commands and usage.
        // The load response owns current configuration; none of the replay
        // can become a live event or a notification on the next turn.
        Ok(())
    }
}
