// SPDX-License-Identifier: MPL-2.0

//! Reading what a CLI's channel currently offers.
//!
//! Two sources, both answering one small JSON document. The parsing is
//! separate from the fetching so a test can feed it the real documents
//! without a registry or a `grok` binary.

use std::time::Duration;

use crate::Version;

/// How long one registry round-trip may take. A check runs on a worker at
/// startup, and the only cost of a slow registry should be a late card.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";

pub fn npm_latest_url(package: &str) -> String {
    format!("{NPM_REGISTRY}/{package}/latest")
}

/// The version named by an npm registry `…/<package>/latest` document.
pub fn parse_npm_latest(body: &str) -> Result<Version, String> {
    let document: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("registry answer: {error}"))?;
    let version = document
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "registry answer names no version".to_owned())?;
    version
        .parse()
        .map_err(|error| format!("registry version {version:?}: {error}"))
}

/// What `grok update --check --json` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokCheck {
    pub current: Option<Version>,
    pub latest: Version,
    pub update_available: bool,
}

pub fn parse_grok_check(body: &str) -> Result<GrokCheck, String> {
    let document: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|error| format!("grok check: {error}"))?;
    if let Some(error) = document.get("error").and_then(|value| value.as_str()) {
        return Err(format!("grok check: {error}"));
    }
    let latest = document
        .get("latestVersion")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "grok check names no latest version".to_owned())?
        .parse()
        .map_err(|error| format!("grok latest version: {error}"))?;
    let current = document
        .get("currentVersion")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse().ok());
    Ok(GrokCheck {
        current,
        latest,
        update_available: document
            .get("updateAvailable")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// One blocking registry request. Built per check rather than held, because a
/// check is daily and a client is not worth keeping for that.
pub fn fetch_npm_latest(package: &str, user_agent: &str) -> Result<Version, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(npm_latest_url(package))
        .send()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("registry returned {}", response.status()));
    }
    let body = response.text().map_err(|error| error.to_string())?;
    parse_npm_latest(&body)
}
