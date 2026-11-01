// SPDX-License-Identifier: MPL-2.0

//! **The nightly app's update notice** (TECHNICAL.final §5.5): two JNI calls
//! on `app.koru.mobile.services.UpdateNative`, marshalling only. Fetching,
//! validating, downloading and verifying are `koru-update`'s — the HTTPS,
//! redirect, manifest and Minisign policy every desktop already runs — so the
//! phone has no second parser and no second verifier.
//!
//! **Outside the mobile operation catalog, on purpose**, as
//! `nativeConfigureAccount` is: the catalog is the surface every platform
//! edge must expose, iOS's included, and only the sideloaded Android nightly
//! updates itself (an iPhone updates through TestFlight, D11).

use jni::objects::{JObject, JString};
use jni::strings::JNIString;
use jni::sys::jstring;
use jni::{Env, EnvUnowned};
use koru_update::{
    FetchResult, ReleaseChannel, UpdateClient, UpdateManifest, UpdateTarget,
    download_verified_package,
};
use std::path::Path;

/// The universal APK's target: the client asks for `android-aarch64`
/// whatever its ABI (A6).
fn target(channel: &str) -> Result<UpdateTarget, String> {
    let channel: ReleaseChannel = channel
        .parse()
        .map_err(|_| "update_channel_invalid".to_owned())?;
    Ok(UpdateTarget {
        channel,
        platform: "android".into(),
        architecture: "aarch64".into(),
    })
}

/// The channel's latest manifest, validated, as JSON for the app to read
/// fields from; `None` when the origin says nothing changed.
pub(crate) fn check(
    base_url: &str,
    channel: &str,
    user_agent: &str,
) -> Result<Option<String>, String> {
    let target = target(channel)?;
    let client = UpdateClient::new(base_url, user_agent)?;
    match client.fetch_manifest(&target, None)? {
        FetchResult::NotModified => Ok(None),
        FetchResult::Manifest { manifest, .. } => serde_json::to_string(&manifest)
            .map(Some)
            .map_err(|error| error.to_string()),
    }
}

/// Download the package `manifest_json` names to `destination` and verify
/// it against `public_key`. The manifest is validated again here: it came
/// back from the app's preferences, and nothing the app stored is trusted
/// more than what the origin served.
pub(crate) fn download(
    channel: &str,
    manifest_json: &str,
    public_key: &str,
    destination: &Path,
    user_agent: &str,
) -> Result<(), String> {
    let manifest = UpdateManifest::parse_and_validate(manifest_json.as_bytes(), &target(channel)?)
        .map_err(|error| error.to_string())?;
    download_verified_package(
        &manifest,
        destination,
        &public_key_box(public_key),
        user_agent,
    )
    .map(|_| ())
}

/// The build carries the key's base64 line alone (a Gradle property is one
/// line); the verifier reads a key box, comment line first.
fn public_key_box(public_key: &str) -> String {
    let key = public_key.trim();
    if key.is_empty() || key.contains('\n') {
        key.to_owned()
    } else {
        format!("untrusted comment: Koru update public key\n{key}\n")
    }
}

fn read(env: &mut Env, value: JString) -> Result<String, String> {
    value
        .mutf8_chars(env)
        .map(|value| value.to_str().into_owned())
        .map_err(|_| "update_argument_invalid".to_owned())
}

fn throw(env: &mut Env, code: &str) -> jni::errors::Result<()> {
    env.throw_new(
        JNIString::new("java/lang/RuntimeException"),
        JNIString::new(code),
    )?;
    Err(jni::errors::Error::JavaException)
}

/// `nativeCheck(baseUrl, channel, userAgent)`: the validated manifest's JSON,
/// or an empty string when there is nothing new; a failure throws, and the
/// app shows nothing for it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_koru_mobile_services_UpdateNative_nativeCheck<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    base_url: JString<'caller>,
    channel: JString<'caller>,
    user_agent: JString<'caller>,
) -> jstring {
    unowned_env
        .with_env(|env| {
            let result = (|| {
                let base_url = read(env, base_url)?;
                let channel = read(env, channel)?;
                let user_agent = read(env, user_agent)?;
                check(&base_url, &channel, &user_agent)
            })();
            match result {
                Ok(json) => env
                    .new_string(json.unwrap_or_default())
                    .map(|value| value.into_raw()),
                Err(code) => throw(env, &code).map(|()| std::ptr::null_mut()),
            }
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `nativeDownload(channel, manifestJson, publicKey, destination,
/// userAgent)`: the verified APK at `destination`, or a throw and nothing
/// there.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_koru_mobile_services_UpdateNative_nativeDownload<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    channel: JString<'caller>,
    manifest_json: JString<'caller>,
    public_key: JString<'caller>,
    destination: JString<'caller>,
    user_agent: JString<'caller>,
) {
    unowned_env
        .with_env(|env| {
            let result = (|| {
                let channel = read(env, channel)?;
                let manifest_json = read(env, manifest_json)?;
                let public_key = read(env, public_key)?;
                let destination = read(env, destination)?;
                let user_agent = read(env, user_agent)?;
                download(
                    &channel,
                    &manifest_json,
                    &public_key,
                    Path::new(&destination),
                    &user_agent,
                )
            })();
            match result {
                Ok(()) => Ok(()),
                Err(code) => throw(env, &code),
            }
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_asks_for_the_universal_apk_target() {
        let nightly = target("nightly").unwrap();
        assert_eq!(nightly.platform, "android");
        assert_eq!(nightly.architecture, "aarch64");
        assert_eq!(
            koru_update::FeedLayout::GitHubRelease.manifest_path(&nightly),
            "nightly-android-aarch64.json"
        );
        assert!(target("not-a-channel").is_err());
    }

    #[test]
    fn a_bare_key_line_becomes_a_key_box() {
        assert_eq!(
            public_key_box(" RWQabc "),
            "untrusted comment: Koru update public key\nRWQabc\n"
        );
        let boxed = "untrusted comment: x\nRWQabc";
        assert_eq!(public_key_box(boxed), boxed);
        assert_eq!(public_key_box(""), "");
    }

    /// A manifest the app kept is checked again before anything downloads:
    /// a desktop manifest, or one for another channel, is refused.
    #[test]
    fn a_stored_manifest_is_validated_again_before_downloading() {
        let dir = std::env::temp_dir().join("koru-update-native-test.apk");
        assert!(download("nightly", "{}", "RWQ", &dir, "t").is_err());
        let desktop = r#"{"schema_version":1,"channel":"nightly","platform":"windows","architecture":"x86_64","version":"0.1.5","build_number":1,"published_at":"now","release_notes":{"title":"t","summary":["s"]},"package":{"url":"https://github.com/x","file_name":"x.zip","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","signature":"s","format":"zip"}}"#;
        let error = download("nightly", desktop, "RWQ", &dir, "t").unwrap_err();
        assert!(error.contains("platform"), "{error}");
        assert!(!dir.exists());
    }
}
