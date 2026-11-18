// SPDX-License-Identifier: MPL-2.0
use super::*;

// A reference names a skill, never embeds its instructions or filesystem path.
pub(super) fn selected(references: &[serde_json::Value]) -> Result<Vec<String>, Error> {
    let names = references
        .iter()
        .filter(|value| value.get("kind").and_then(|kind| kind.as_str()) == Some("skill"))
        .map(|value| {
            value
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_owned)
                .ok_or(Error::InvalidRequest)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !koru_session_protocol::valid_input_skills(&names) {
        return Err(Error::InvalidRequest);
    }
    Ok(names)
}

/// Only owed work activates skills. Context from earlier turns is history.
pub(crate) fn work_skills(delivery: &Delivery) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for message in &delivery.work {
        for name in selected(&message.content.references)? {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    if !koru_session_protocol::valid_input_skills(&names) {
        return Err(Error::InputTooSmall);
    }
    Ok(names)
}
