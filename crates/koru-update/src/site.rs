// SPDX-License-Identifier: MPL-2.0

//! Update checks routed through `www.koru.do`, so the site can count which
//! builds are asking.
//!
//! **The site only redirects; nothing about trust moves.** The site answers
//! `/v1/app/update/channels/<channel>/<platform>-<arch>.json` with a 302 to the
//! same manifest this build would have fetched from its own origin
//! (`updates.koru.do`, or the GitHub nightly release directory on Linux), and
//! the client then checks the manifest's target, the package hash and its
//! Minisign signature exactly as before. The site records the hit
//! (version from the user agent, channel, platform, architecture) before it
//! redirects.
//!
//! **Only production origins are routed.** A build pointed at a local feed or
//! a rehearsal through `KORU_UPDATE_BASE_URL` talks to that feed directly,
//! because the site cannot redirect to an origin it does not know.
//!
//! **The site failing never costs an update.** Any error from the site leg
//! (no connection, a timeout, a 5xx, a 404, a body that is not a manifest)
//! falls back to the origin directly, in the same check.
//!
//! **`first_today` is the one thing added**, and it carries no identity: it
//! is `1` on the first check of a UTC day that reached the site, so the site
//! can count daily active installs per version without an install id. The
//! day is remembered in [`crate::UpdateState::site_counted_day`] only when the
//! site leg answered, so a failed check does not use up the day's count.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::Url;

use crate::manifest::FeedLayout;
use crate::{FetchResult, UpdateClient, UpdateTarget};

/// The site's update route. Always the Channels layout beneath it.
pub const SITE_UPDATE_BASE: &str = "https://www.koru.do/v1/app/update/";

/// The origins the site knows how to redirect to. Anything else, a local feed
/// or a rehearsal, is fetched directly.
const PRODUCTION_ORIGINS: &[&str] = &[
    "https://updates.koru.do/",
    "https://github.com/koruworks/mono/releases/download/nightly/",
];

/// The connect timeout for the site leg. Shorter than the origin's own five
/// seconds: an unreachable site should cost little before the fallback.
pub const SITE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether `origin` is a production feed the site redirects to.
pub fn routes_through_site(origin: &str) -> bool {
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    PRODUCTION_ORIGINS.iter().any(|known| {
        Url::parse(known).is_ok_and(|known| {
            // A base written without its trailing slash names the same feed.
            let trim = |url: &Url| url.as_str().trim_end_matches('/').to_owned();
            trim(&known) == trim(&origin)
        })
    })
}

/// The current UTC day number since the Unix epoch.
pub fn utc_day(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() / 86_400
}

/// Whether a check on `today` is the first this install sends the site today.
pub fn first_today(counted_day: Option<u64>, today: u64) -> bool {
    counted_day != Some(today)
}

/// The manifest URL on the site for `target`, with `first_today=1` when set.
pub fn site_manifest_url(
    base: &str,
    target: &UpdateTarget,
    first_today: bool,
) -> Result<Url, String> {
    let base = Url::parse(base).map_err(|e| e.to_string())?;
    let mut url = base
        .join(&FeedLayout::Channels.manifest_path(target))
        .map_err(|e| e.to_string())?;
    if first_today {
        url.query_pairs_mut().append_pair("first_today", "1");
    }
    Ok(url)
}

/// What one check came to, and whether the site answered it.
#[derive(Debug)]
pub struct SiteCheck {
    pub result: Result<FetchResult, String>,
    /// The site leg answered (a manifest or a 304). Only then has the site
    /// counted this check, so only then is the day used up.
    pub site_answered: bool,
}

/// Check for an update, through the site first when `origin` is a production
/// feed, and from `origin` directly otherwise or when the site leg fails.
pub fn check(
    origin: &str,
    user_agent: &str,
    target: &UpdateTarget,
    etag: Option<&str>,
    first_today: bool,
) -> SiteCheck {
    let site = routes_through_site(origin).then_some(SITE_UPDATE_BASE);
    check_via(site, origin, user_agent, target, etag, first_today)
}

/// [`check`] with the site base supplied, so a test can stand in for it.
pub fn check_via(
    site: Option<&str>,
    origin: &str,
    user_agent: &str,
    target: &UpdateTarget,
    etag: Option<&str>,
    first_today: bool,
) -> SiteCheck {
    if let Some(site) = site {
        let primary = UpdateClient::with_connect_timeout(site, user_agent, SITE_CONNECT_TIMEOUT)
            .and_then(|client| {
                let url = site_manifest_url(site, target, first_today)?;
                client.fetch_manifest_at(url, target, etag)
            });
        if let Ok(result) = primary {
            return SiteCheck {
                result: Ok(result),
                site_answered: true,
            };
        }
    }
    SiteCheck {
        result: UpdateClient::new(origin, user_agent)
            .and_then(|client| client.fetch_manifest(target, etag)),
        site_answered: false,
    }
}

#[cfg(test)]
mod tests;
