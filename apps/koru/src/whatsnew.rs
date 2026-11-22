// SPDX-License-Identifier: MPL-2.0

//! What a build says about itself, the first time you see it.
//!
//! An update used to finish in silence. The banner went away, the windows came
//! back, and the only evidence that anything had happened was a version string
//! in a tray submenu nobody opens — after the person had agreed to two prompts
//! and watched their work close. So a build that has just been installed opens
//! one tab and says so.
//!
//! ## The notes are on the disk, not on the wire
//!
//! `UpdateManifest.release_notes` carries a title, a few summary lines and a
//! URL, and never the body: the long Markdown is rendered to HTML once, by
//! `koru-release`, and lives on R2 as an immutable page. That page is right for
//! a link somebody shares and useless here.
//!
//! So `koru-release` writes the same Markdown into the package as
//! [`RELEASE_NOTES_FILE`], beside the four executables, and this reads it from
//! beside *its own* executable. The build that is running always has the notes
//! for the build that is running — no network, no manifest change, and no
//! version to match up, because a file cannot be beside the wrong binary.
//!
//! **Absence is ordinary.** A `cargo run` build was never released and a
//! `dist-local` bundle usually has no descriptor, so there is frequently no file
//! at all. The greeting stands on its own and nothing logs a complaint.

use koru_channel::RELEASE_NOTES_FILE;
use koru_workspace_host::{RichBlock, parse_markdown};

/// The GitHub releases list, including release discussions and reactions.
pub const RELEASES_URL: &str = "https://github.com/koruworks/mono/releases";

/// A Help destination on the site. No install, account or device id is put
/// in a browser URL. The site allowlists the destination and returns 302.
pub fn app_link(action: &str, tag: Option<&str>) -> String {
    let info = koru_channel::build_info();
    let mut url = url::Url::parse("https://www.koru.do/go/").expect("fixed site URL");
    url.path_segments_mut()
        .expect("site path")
        .pop_if_empty()
        .push(action);
    url.query_pairs_mut()
        .append_pair("v", info.version)
        .append_pair("ch", koru_channel::ReleaseChannel::of_this_build().as_str())
        .append_pair("os", info.platform);
    if let Some(tag) = tag {
        url.query_pairs_mut().append_pair("tag", tag);
    }
    url.into()
}

pub fn release_link(version: &str, channel: koru_channel::ReleaseChannel) -> String {
    use koru_channel::ReleaseChannel;
    match channel {
        ReleaseChannel::Internal => app_link("releases", None),
        ReleaseChannel::Nightly => app_link("release", Some("nightly")),
        _ => app_link("release", Some(&format!("v{version}"))),
    }
}

/// This build's release notes, if it shipped with any.
///
/// **Beside the executable, never through a search path.** The rule the four
/// binaries already follow, for the reason it exists: they are built, signed and
/// shipped as one unit, and a `RELEASE_NOTES.md` found anywhere else describes
/// some other build.
///
/// Every failure is `None` — no executable path, no parent, no file, unreadable,
/// not UTF-8. None of them is worth a word to anybody: the tab still opens and
/// still says what happened.
///
/// **Read once, when the tab is created**, which is once per install. Not on a
/// timer and not per frame, so it is outside everything the responsiveness rules
/// are about.
pub fn notes_beside_the_exe() -> Option<String> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Beside the exe on Windows; in the bundle's Resources/ on macOS, where
    // codesign's seal keeps data out of Contents/MacOS. Still this build's
    // own notes either way — both paths are inside the shipped unit.
    std::fs::read_to_string(dir.join(RELEASE_NOTES_FILE))
        .ok()
        .or_else(|| {
            std::fs::read_to_string(dir.parent()?.join("Resources").join(RELEASE_NOTES_FILE)).ok()
        })
}

/// The first thing the tab says, as Markdown.
///
/// **A link rather than a bare address**, so clicking it in the page opens the
/// releases site. In-app Feedback is the overflow-menu destination.
///
/// English, like the notes underneath it. `koru-i18n` covers the first-run
/// screens and nothing else, and a translated sentence sitting on top of
/// untranslated release notes reads worse than a page that is consistently one
/// language.
pub fn greeting(version: &str) -> String {
    format!(
        "Updated to Koru {version}. See what changed on the [releases page]({url}). \
         Feedback is always welcome — use Feedback in the app menu.",
        url = "https://www.koru.do/releases",
    )
}

/// The whole page: the greeting, then this build's notes if it has any.
///
/// **Two parses, concatenated — not one string parsed once.** Joining them with
/// a blank line and handing the result to one parser is the obvious
/// simplification and it is wrong for a handful of openings: notes that begin
/// with a setext underline (`===`) would retroactively turn the greeting's
/// paragraph into a heading, and a body opening inside a fence would swallow it
/// whole. The greeting is ours and the notes are the release author's; parsing
/// them separately is what keeps one from reinterpreting the other.
pub fn blocks(version: &str, notes: Option<&str>) -> Vec<RichBlock> {
    let mut blocks = parse_markdown(&greeting(version));
    if let Some(notes) = notes {
        blocks.extend(parse_markdown(notes));
    }
    blocks
}

/// The notes' section headings, in order — the `## ` lines of the shipped
/// body. The help popover shows the first three as *What's new*
/// (`docs/features/workspace-foundation/TECHNICAL.final.md` §5.10): the
/// shipped file is one version's notes with no dates and no earlier
/// versions, so its sections are the honest list.
pub fn headlines(notes: &str) -> Vec<String> {
    notes
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let rest = line.strip_prefix("## ")?;
            let rest = rest.trim();
            (!rest.is_empty()).then(|| rest.to_owned())
        })
        .collect()
}

/// Read the shipped notes **off the event thread**, once, and hand back
/// their headlines — `None` when no notes shipped beside this build. The
/// window keeps the answer; the `?` never reads the disk.
pub fn read_headlines_async() -> std::sync::mpsc::Receiver<Option<Vec<String>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("koru-whatsnew-headlines".into())
        .spawn(move || {
            let answer = notes_beside_the_exe().map(|notes| headlines(&notes));
            let _ = tx.send(answer);
        })
        .ok();
    rx
}

/// One published release, as the Help popover lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRelease {
    /// `None` for the moving nightly; the version (`0.1.5`) otherwise.
    pub version: Option<String>,
    /// Its GitHub page — always under [`RELEASES_URL`], checked when read.
    pub url: String,
    /// The publication day, UTC. For the moving nightly, use the newest
    /// completed asset upload: the release's own date stays at its creation.
    pub published: Option<(i32, u8, u8)>,
}

/// The releases list as data. Unauthenticated, so 60 requests an hour per
/// address — one per window, and only once its Help popover opens.
const RELEASES_API: &str = "https://api.github.com/repos/koruworks/mono/releases?per_page=20";
const RELEASES_FEED: &str = "https://github.com/koruworks/mono/releases.atom";

/// How many stable releases follow the nightly. Older ones are a press away
/// on *View all releases*.
const STABLE_SHOWN: usize = 3;

fn version_key(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let key = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(key)
}

fn release_day(stamp: &str) -> Option<(i32, u8, u8)> {
    use chrono::Datelike;
    let date = chrono::DateTime::parse_from_rfc3339(stamp).ok()?.to_utc();
    Some((date.year(), date.month() as u8, date.day() as u8))
}

fn latest_releases(mut releases: Vec<PublishedRelease>) -> Vec<PublishedRelease> {
    releases.sort_by_key(|release| {
        (
            release.version.is_some(),
            std::cmp::Reverse(release.version.as_deref().and_then(version_key)),
        )
    });
    releases.dedup_by(|a, b| a.version == b.version);
    let has_nightly = releases.first().is_some_and(|r| r.version.is_none());
    releases.truncate(STABLE_SHOWN + usize::from(has_nightly));
    releases
}

/// **The actual releases** (the user, 2026-11-10): the nightly first — it
/// is always the newest build and a different kind of thing — then stable
/// versions newest first. Drafts, other prereleases, tags that are not
/// `v<major>.<minor>.<patch>`, and any entry whose page is not this
/// repository's release page are left out, so a row can only ever open a
/// Koru release. `None` when the body is not a releases list at all.
pub fn parse_releases(json: &str) -> Option<Vec<PublishedRelease>> {
    #[derive(serde::Deserialize)]
    struct Raw {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
        published_at: Option<String>,
        #[serde(default)]
        assets: Vec<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        state: String,
        created_at: Option<String>,
    }
    let raw: Vec<Raw> = serde_json::from_str(json).ok()?;
    let mut releases = Vec::new();
    for release in raw {
        if release.draft || release.html_url != format!("{RELEASES_URL}/tag/{}", release.tag_name) {
            continue;
        }
        if release.tag_name == "nightly" {
            releases.push(PublishedRelease {
                version: None,
                url: release.html_url,
                published: release
                    .assets
                    .iter()
                    .filter(|asset| asset.state == "uploaded")
                    .filter_map(|asset| asset.created_at.as_deref().and_then(release_day))
                    .max(),
            });
            continue;
        }
        let Some(version) = release.tag_name.strip_prefix('v') else {
            continue;
        };
        if release.prerelease || version_key(version).is_none() {
            continue;
        }
        releases.push(PublishedRelease {
            version: Some(version.to_owned()),
            url: release.html_url.clone(),
            published: release.published_at.as_deref().and_then(release_day),
        });
    }
    Some(latest_releases(releases))
}

/// The public Atom feed is independent of the REST API's shared-IP quota.
/// Nightly's publisher edits the release after uploading, so its `updated`
/// date describes the current nightly. Stable entries have no publication
/// timestamp here: leave that date unknown instead of calling an edit a release.
fn parse_releases_feed(xml: &str) -> Option<Vec<PublishedRelease>> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(xml);
    let mut releases = Vec::new();
    let mut in_entry = false;
    let mut url = String::new();
    let mut updated = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) if e.name().as_ref() == b"entry" => {
                in_entry = true;
                url.clear();
                updated = None;
            }
            Event::Start(e) if in_entry && e.name().as_ref() == b"updated" => {
                updated = release_day(&reader.read_text(e.name()).ok()?.decode().ok()?);
            }
            Event::Empty(e) if in_entry && e.name().as_ref() == b"link" => {
                for attr in e.attributes() {
                    let attr = attr.ok()?;
                    if attr.key.as_ref() == b"href" {
                        url = attr
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .ok()?
                            .into_owned();
                    }
                }
            }
            Event::End(e) if e.name().as_ref() == b"entry" => {
                in_entry = false;
                let Some(tag) = url.strip_prefix(&format!("{RELEASES_URL}/tag/")) else {
                    continue;
                };
                let version = if tag == "nightly" {
                    None
                } else if let Some(version) =
                    tag.strip_prefix('v').filter(|v| version_key(v).is_some())
                {
                    Some(version.to_owned())
                } else {
                    continue;
                };
                releases.push(PublishedRelease {
                    published: if version.is_none() { updated } else { None },
                    version,
                    url: url.clone(),
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (!releases.is_empty()).then(|| latest_releases(releases))
}

/// Read the releases list off the window's thread. **Nothing about the
/// person goes out**: build dimensions only, no token or installation id.
/// The site counts the request and serves a cached GitHub release list. Any failure — offline, rate-limited, a changed body — is `None`,
/// and the popover keeps the row it had for this build.
pub fn fetch_releases_async() -> std::sync::mpsc::Receiver<Option<Vec<PublishedRelease>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if cfg!(test) {
        // Menu tests inject responses; opening Help must not use a real quota.
        return rx;
    }
    std::thread::Builder::new()
        .name("koru-help-releases".into())
        .spawn(move || {
            let answer = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent(format!(
                    "Koru/{} (build {})",
                    koru_channel::build_info().version,
                    koru_channel::build_info().build_number
                ))
                .build()
                .ok()
                .and_then(|client| {
                    let mut site = url::Url::parse("https://www.koru.do/v1/app/releases").ok()?;
                    let info = koru_channel::build_info();
                    site.query_pairs_mut()
                        .append_pair("v", info.version)
                        .append_pair("ch", koru_channel::ReleaseChannel::of_this_build().as_str())
                        .append_pair("os", info.platform);
                    fetch_release_list(&client, site.as_str(), parse_releases)
                        .or_else(|| fetch_release_list(&client, RELEASES_API, parse_releases))
                        .or_else(|| fetch_release_list(&client, RELEASES_FEED, parse_releases_feed))
                });
            let _ = tx.send(answer);
        })
        .ok();
    rx
}

fn fetch_release_list(
    client: &reqwest::blocking::Client,
    url: &str,
    parse: fn(&str) -> Option<Vec<PublishedRelease>>,
) -> Option<Vec<PublishedRelease>> {
    let result = client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text());
    match result {
        Ok(body) => {
            let releases = parse(&body).filter(|list| !list.is_empty());
            if releases.is_none() {
                tracing::warn!(url, "Help release list contained no usable releases");
            }
            releases
        }
        Err(error) => {
            tracing::warn!(url, %error, "Could not load Help releases");
            None
        }
    }
}

#[cfg(test)]
mod tests;
