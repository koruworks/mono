// SPDX-License-Identifier: MPL-2.0

//! **One package, downloaded and proven, for a client that installs it some
//! other way**: the Android nightly, which hands the verified APK to the
//! system's package installer (TECHNICAL.final §5.5, rung 3).
//!
//! The policy is the desktop updater's, word for word: HTTPS only, up to five
//! redirects that stay on HTTPS (a GitHub asset is always one hop away), the
//! declared size as a hard ceiling while the bytes arrive, and then
//! [`verify_package`] — byte count, SHA-256 and the Minisign signature
//! against the key the build carries. The desktop controller keeps its own
//! download because it also reports progress, cancels, and fills its
//! verified-package cache; none of that is needed to hand one file on.
//!
//! **Nothing unverified is left where a reader would take it**: the bytes go
//! to `<destination>.part` and are renamed only after they verify, and a
//! failure removes the part file.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::CONTENT_LENGTH;
use url::Url;

use crate::{PackageVerification, UpdateManifest, verify_package};

/// Download `manifest`'s package to `destination` and verify it with
/// `public_key`, or fail and leave nothing at `destination`.
pub fn download_verified_package(
    manifest: &UpdateManifest,
    destination: &Path,
    public_key: &str,
    user_agent: &str,
) -> Result<PackageVerification, String> {
    if public_key.trim().is_empty() {
        return Err("this build carries no update public key".into());
    }
    let url = Url::parse(&manifest.package.url).map_err(|e| e.to_string())?;
    if url.scheme() != "https" && !loopback_allowed(&url) {
        return Err("package URL must use HTTPS".into());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = part_path(destination);
    let result = download_to(manifest, &url, &part, user_agent)
        .and_then(|()| verify_package(&part, &manifest.package, public_key))
        .and_then(|verified| {
            fs::rename(&part, destination).map_err(|e| e.to_string())?;
            Ok(verified)
        });
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn download_to(
    manifest: &UpdateManifest,
    url: &Url,
    part: &Path,
    user_agent: &str,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30 * 60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5
                || (attempt.url().scheme() != "https" && !loopback_allowed(attempt.url()))
            {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(url.clone()).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("package request returned {}", response.status()));
    }
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length != manifest.package.size
    {
        return Err(format!(
            "server content length is {length}, expected {}",
            manifest.package.size
        ));
    }
    let mut output = fs::File::create(part).map_err(|e| e.to_string())?;
    let mut received = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        received = received
            .checked_add(read as u64)
            .ok_or("download byte-count overflow")?;
        if received > manifest.package.size {
            return Err("download exceeded the declared package size".into());
        }
        output
            .write_all(&buffer[..read])
            .map_err(|e| e.to_string())?;
    }
    output.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    destination.with_file_name(name)
}

/// A loopback origin over plain HTTP, in a debug build only — the rehearsal
/// feed the desktop controller allows too, and what the tests below serve.
fn loopback_allowed(url: &Url) -> bool {
    cfg!(debug_assertions) && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
}

#[cfg(test)]
mod tests;
