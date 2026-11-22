// SPDX-License-Identifier: MPL-2.0

use reqwest::blocking::Client;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use reqwest::{StatusCode, redirect};
use std::time::Duration;
use url::Url;

use crate::{UpdateManifest, UpdateTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchResult {
    NotModified,
    Manifest {
        manifest: UpdateManifest,
        etag: Option<String>,
    },
}

#[derive(Clone)]
pub struct UpdateClient {
    client: Client,
    base_url: Url,
}

impl UpdateClient {
    pub fn new(base_url: &str, user_agent: &str) -> Result<Self, String> {
        Self::with_connect_timeout(base_url, user_agent, Duration::from_secs(5))
    }

    /// [`Self::new`] with its own connect timeout, for the site leg of a check
    /// ([`crate::site`]), which should give up sooner than the origin.
    pub fn with_connect_timeout(
        base_url: &str,
        user_agent: &str,
        connect_timeout: Duration,
    ) -> Result<Self, String> {
        let base_url = Url::parse(base_url).map_err(|e| e.to_string())?;
        if base_url.scheme() != "https" && !debug_override_allowed(&base_url) {
            return Err("the update origin must use HTTPS".into());
        }
        // A GitHub release asset is always one redirect away: `github.com`
        // answers 302 to a signed, short-lived URL on its asset host, over
        // HTTPS, and that host answers the ETag and a conditional 304 —
        // `If-None-Match` survives the hop, since only credentials are
        // stripped when a redirect changes host. Observed 2026-10-24.
        let policy = redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });
        let client = Client::builder()
            .user_agent(user_agent)
            .connect_timeout(connect_timeout)
            .timeout(Duration::from_secs(15))
            .redirect(policy)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, base_url })
    }

    pub fn fetch_manifest(
        &self,
        target: &UpdateTarget,
        etag: Option<&str>,
    ) -> Result<FetchResult, String> {
        let url = target.manifest_url(&self.base_url)?;
        self.fetch_manifest_at(url, target, etag)
    }

    /// Fetch the manifest for `target` from exactly `url`. The site's route
    /// carries a query the origin's layout cannot express, so its URL is
    /// built by [`crate::site`] rather than from the base.
    pub fn fetch_manifest_at(
        &self,
        url: Url,
        target: &UpdateTarget,
        etag: Option<&str>,
    ) -> Result<FetchResult, String> {
        if url.scheme() != "https" && !debug_override_allowed(&url) {
            return Err("the update origin must use HTTPS".into());
        }
        let mut request = self.client.get(url);
        if let Some(value) = etag {
            request = request.header(IF_NONE_MATCH, value);
        }
        let response = request.send().map_err(|e| e.to_string())?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(FetchResult::NotModified);
        }
        if !response.status().is_success() {
            return Err(format!("manifest request returned {}", response.status()));
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = response.bytes().map_err(|e| e.to_string())?;
        let manifest =
            UpdateManifest::parse_and_validate(&bytes, target).map_err(|e| e.to_string())?;
        Ok(FetchResult::Manifest { manifest, etag })
    }

    pub fn blocking(&self) -> &Client {
        &self.client
    }
}

fn debug_override_allowed(url: &Url) -> bool {
    cfg!(debug_assertions) && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
}
