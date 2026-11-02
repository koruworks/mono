// SPDX-License-Identifier: MPL-2.0

//! Exact 0.76.0 wrapper policy. Extension payloads never leave this module.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum WrapperAuth {
    Authenticated,
    SignedOut,
    Unverified,
}

pub(super) fn auth_status(params: &Value) -> WrapperAuth {
    match params.pointer("/authStatus/kind").and_then(Value::as_str) {
        Some("account" | "api_key") => WrapperAuth::Authenticated,
        Some("none") => WrapperAuth::SignedOut,
        // A configured external cloud backend is not proof of credentials;
        // this route never performs ACP gateway authentication.
        _ => WrapperAuth::Unverified,
    }
}

pub(super) fn validate_handshake(initialized: &Value) -> Result<(), ProviderFailure> {
    if initialized
        .pointer("/agentInfo/version")
        .and_then(Value::as_str)
        != Some("0.76.0")
        || !initialized
            .pointer("/agentCapabilities/_meta/authStatus")
            .is_some_and(Value::is_object)
    {
        return Err(handshake_rejected());
    }
    Ok(())
}

impl AcpControl {
    pub(super) fn await_wrapper_auth(&mut self) -> Result<(), ProviderFailure> {
        let deadline = Instant::now() + self.config.request_timeout;
        loop {
            match self.wrapper_auth {
                Some(WrapperAuth::Authenticated) => return Ok(()),
                Some(WrapperAuth::SignedOut) => {
                    return Err(failure(
                        ErrorCode::ProviderUnavailable,
                        ProviderStage::Initialize,
                        ProviderDiagnostic::AuthenticationRequired,
                    ));
                }
                Some(WrapperAuth::Unverified) => {
                    return Err(failure(
                        ErrorCode::ProviderFailed,
                        ProviderStage::Initialize,
                        ProviderDiagnostic::HandshakeRejected,
                    ));
                }
                None => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(
                    ErrorCode::ProviderFailed,
                    ProviderStage::Initialize,
                    ProviderDiagnostic::Timeout,
                ));
            }
            match self.transport()?.receive(remaining)? {
                TransportReceive::Frame(frame) => {
                    self.diagnostics.frames_read = self.diagnostics.frames_read.saturating_add(1);
                    self.handle_frame(frame)?;
                }
                TransportReceive::Timeout => {
                    return Err(failure(
                        ErrorCode::ProviderFailed,
                        ProviderStage::Initialize,
                        ProviderDiagnostic::Timeout,
                    ));
                }
                TransportReceive::Woken => {}
            }
        }
    }
}

/// How the wrapper spells a model's 1M-context sibling: `opus[1m]` beside
/// `opus`. The spelling stays in this module; above it a model carries its
/// longer window as a context choice.
const ONE_MILLION_SUFFIX: &str = "[1m]";
const ONE_MILLION_TOKENS: u32 = 1_000_000;

/// **One model with a context choice, not two models** (koru-ai D10). The
/// wrapper lists a model and its 1M-context sibling side by side (`opus`,
/// `opus[1m]`, the latter named *Opus (1M context)*). The sibling is folded
/// into its base, in the same group, as that model's 1M context: the base
/// keeps its own name and description and takes the earlier of the two
/// places, and the levels of whichever the session described when the base
/// has none. A sibling whose base is not listed stays a model of its own.
pub(super) fn fold_context_siblings(
    models: Vec<koru_session_protocol::ProviderModel>,
) -> Vec<koru_session_protocol::ProviderModel> {
    // Each sibling's base, by index: the model it names, in its group.
    let bases: Vec<Option<usize>> = models
        .iter()
        .map(|model| {
            let id = model.id.as_str();
            let end = id.len().checked_sub(ONE_MILLION_SUFFIX.len())?;
            let base = id
                .get(end..)
                .filter(|suffix| suffix.eq_ignore_ascii_case(ONE_MILLION_SUFFIX))
                .and(id.get(..end))
                .filter(|base| !base.is_empty())?;
            models.iter().position(|candidate| {
                candidate.id.as_str() == base && candidate.group == model.group
            })
        })
        .collect();
    let mut out: Vec<koru_session_protocol::ProviderModel> = Vec::with_capacity(models.len());
    // Where each kept model went in `out`, by its index in `models`.
    let mut placed = BTreeMap::new();
    for (index, model) in models.iter().enumerate() {
        let kept = bases[index].unwrap_or(index);
        let position = *placed.entry(kept).or_insert_with(|| {
            out.push(models[kept].clone());
            out.len() - 1
        });
        if bases[index].is_some() {
            let base = &mut out[position];
            base.context = Some(koru_session_protocol::ModelContext {
                id: model.id.clone(),
                tokens: ONE_MILLION_TOKENS,
            });
            if base.reasoning.is_none() {
                base.reasoning = model.reasoning.clone();
            }
        }
    }
    out
}

/// Extend, never replace, caller restrictions. Invalid metadata cannot discard
/// the mandatory exclusions silently.
pub(super) fn enforce_child_exclusions(meta: &mut Value) -> Result<(), ProviderFailure> {
    let invalid = || {
        failure(
            ErrorCode::InvalidRequest,
            ProviderStage::Initialize,
            ProviderDiagnostic::InvalidInput,
        )
    };
    if !meta.is_object() {
        return Err(invalid());
    }
    if meta.get("claudeCode").is_none() {
        meta["claudeCode"] = json!({});
    }
    let claude = meta
        .get_mut("claudeCode")
        .filter(|v| v.is_object())
        .ok_or_else(invalid)?;
    if claude.get("options").is_none() {
        claude["options"] = json!({});
    }
    let options = claude
        .get_mut("options")
        .filter(|v| v.is_object())
        .ok_or_else(invalid)?;
    if options.get("disallowedTools").is_none() {
        options["disallowedTools"] = json!([]);
    }
    let disallowed = options
        .get_mut("disallowedTools")
        .and_then(Value::as_array_mut)
        .ok_or_else(invalid)?;
    if !disallowed.iter().all(Value::is_string) {
        return Err(invalid());
    }
    for name in ["Agent", "Task"] {
        if !disallowed.iter().any(|v| v.as_str() == Some(name)) {
            disallowed.push(json!(name));
        }
    }
    Ok(())
}
