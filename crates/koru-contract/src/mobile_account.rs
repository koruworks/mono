// SPDX-License-Identifier: MPL-2.0

//! Bounds for the private platform account edge. Unknown actions fail closed;
//! unknown protobuf fields may be ignored without changing command semantics.
use crate::mobile_proto as wire;

pub const MAX_TOKEN_BYTES: usize = 16 * 1024;
pub const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

pub fn command(value: &wire::AccountCommandRequest) -> Result<(), &'static str> {
    envelope(value.version, value.requester)?;
    let action =
        wire::AccountAction::try_from(value.action).map_err(|_| "account_action_unknown")?;
    if action == wire::AccountAction::Unspecified {
        return Err("account_action_unknown");
    }
    if value.locale.len() > 64 || value.profile_id.len() > 36 || value.account_id.len() > 36 {
        return Err("account_payload_too_large");
    }
    if !matches!(
        action,
        wire::AccountAction::SwitchAccount | wire::AccountAction::AddAccount
    ) && value.expected_owner_scope.is_none()
    {
        return Err("account_scope_missing");
    }
    if action == wire::AccountAction::SwitchAccount {
        let id =
            uuid::Uuid::parse_str(&value.account_id).map_err(|_| "account_identity_invalid")?;
        if id.get_version_num() != 4
            || id.get_variant() != uuid::Variant::RFC4122
            || id.to_string() != value.account_id
        {
            return Err("account_identity_invalid");
        }
    } else if !value.account_id.is_empty() {
        return Err("account_payload_invalid");
    }
    let inventory = matches!(
        action,
        wire::AccountAction::InspectCloudUsageWorkspaces
            | wire::AccountAction::InspectCloudUsageDocuments
    );
    if inventory {
        if value.cloud_cursor.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 2048
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) {
            return Err("account_payload_invalid");
        }
        if action == wire::AccountAction::InspectCloudUsageDocuments {
            for value in [&value.cloud_workspace_id, &value.cloud_incarnation_id] {
                let value = value.as_deref().ok_or("account_payload_invalid")?;
                if value.len() != 36 {
                    return Err("account_payload_invalid");
                }
                let id = uuid::Uuid::parse_str(value).map_err(|_| "account_payload_invalid")?;
                if id.to_string() != value {
                    return Err("account_payload_invalid");
                }
            }
        } else if value.cloud_workspace_id.is_some() || value.cloud_incarnation_id.is_some() {
            return Err("account_payload_invalid");
        }
    } else if value.cloud_workspace_id.is_some()
        || value.cloud_incarnation_id.is_some()
        || value.cloud_cursor.is_some()
    {
        return Err("account_payload_invalid");
    }
    let inspection = matches!(
        action,
        wire::AccountAction::InspectCloudDocument | wire::AccountAction::RemoveCloudFile
    );
    if inspection {
        let selection = value
            .cloud_document_selection_json
            .as_deref()
            .ok_or("account_payload_invalid")?;
        if selection.is_empty() || selection.len() > 1024 {
            return Err("account_payload_invalid");
        }
        if action == wire::AccountAction::RemoveCloudFile {
            let node = value
                .cloud_file_node_id
                .as_deref()
                .ok_or("account_payload_invalid")?;
            if node.len() != 36
                || uuid::Uuid::parse_str(node)
                    .ok()
                    .is_none_or(|id| id.to_string() != node)
            {
                return Err("account_payload_invalid");
            }
        } else if value.cloud_file_node_id.is_some() {
            return Err("account_payload_invalid");
        }
    } else if value.cloud_document_selection_json.is_some() || value.cloud_file_node_id.is_some() {
        return Err("account_payload_invalid");
    }
    if action == wire::AccountAction::Cancel && value.generation == 0 {
        return Err("account_generation_invalid");
    }
    // The switch names exactly one Profile, so its id is required rather than
    // optional: a set command with no target would otherwise read as "every
    // Profile", which is not a thing a person can ask for.
    if action == wire::AccountAction::SetProfileCloudSync {
        if value.profile_id.is_empty() || value.cloud_sync_enabled.is_none() {
            return Err("account_payload_invalid");
        }
        validate_profile_id(&value.profile_id)?;
    } else if value.cloud_sync_enabled.is_some() {
        return Err("account_payload_invalid");
    } else if action == wire::AccountAction::RetryEnrollment && !value.profile_id.is_empty() {
        validate_profile_id(&value.profile_id)?;
    } else if !value.profile_id.is_empty() {
        return Err("account_payload_invalid");
    }
    if action == wire::AccountAction::SelectProfiles {
        let mut previous: Option<&str> = None;
        for profile in &value.profile_ids {
            let id = uuid::Uuid::parse_str(profile).map_err(|_| "account_profile_invalid")?;
            if id.get_version_num() != 4
                || id.get_variant() != uuid::Variant::RFC4122
                || id.to_string() != *profile
                || previous.is_some_and(|previous| previous >= profile.as_str())
            {
                return Err("account_profile_invalid");
            }
            previous = Some(profile);
        }
    } else if !value.profile_ids.is_empty() {
        return Err("account_payload_invalid");
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> Result<(), &'static str> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| "account_profile_invalid")?;
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.to_string() != value
    {
        return Err("account_profile_invalid");
    }
    Ok(())
}

pub fn envelope(version: u32, requester: u64) -> Result<(), &'static str> {
    if version != 1 {
        return Err("account_version_unsupported");
    }
    if requester == 0 {
        return Err("account_requester_invalid");
    }
    Ok(())
}

pub fn platform(value: &wire::AccountPlatformRequest) -> Result<(), &'static str> {
    envelope(value.version, value.requester)?;
    use wire::account_platform_request::Response;
    match value.response.as_ref().ok_or("account_response_missing")? {
        Response::SignIn(reply) => {
            if reply.request_id == 0 || reply.generation == 0 {
                return Err("account_correlation_invalid");
            }
            if reply
                .id_token
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > MAX_TOKEN_BYTES)
                || (reply.cancelled && reply.id_token.is_some())
            {
                return Err("account_payload_invalid");
            }
        }
        Response::Vault(reply) => {
            if reply.request_id == 0 {
                return Err("account_correlation_invalid");
            }
            if reply
                .credential
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > MAX_CREDENTIAL_BYTES)
                || (!reply.success && reply.credential.is_some())
            {
                return Err("account_payload_invalid");
            }
        }
        Response::Browser(reply) => {
            if reply.request_id == 0 || reply.generation == 0 {
                return Err("account_correlation_invalid");
            }
            if reply
                .callback_url
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > MAX_TOKEN_BYTES)
                || reply.cancelled == reply.callback_url.is_some()
            {
                return Err("account_payload_invalid");
            }
        }
        Response::Foreground(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
