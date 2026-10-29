// SPDX-License-Identifier: MPL-2.0

//! **Putting an icon and a version block inside a Windows executable.**
//!
//! A build-dependency, used from a `build.rs`. It writes a resource script,
//! finds a resource compiler, runs it, and emits the link argument — the four
//! steps that are identical for every binary in the tree and that nobody wants
//! to debug twice.
//!
//! ## Why it is a crate rather than a copied build script
//!
//! [`find_rc`] is the reason. Locating `rc.exe` means walking
//! `%ProgramFiles(x86)%\Windows Kits\10\bin`, sorting version directories
//! numerically, picking the newest that has **both** a compiler for this host
//! architecture and the matching `Include` tree — because the two are versioned
//! together and one without the other fails at resource-compile time rather
//! than here, with a message about `winver.h` that says nothing about SDKs.
//! That is sixty lines nobody should own two copies of.
//!
//! ## The rule this crate exists to keep
//!
//! **`file_description` must not contain a version.** Windows shows it in the
//! taskbar's jump list header, the button tooltip, Task Manager and — the one
//! that bites — the notification area's per-app *"always show this icon"* list.
//! That setting is remembered against the string, so a description carrying a
//! version counts as a different application on every build and the tray icon
//! silently disappears. It cost a real bug once already. The version still
//! travels, in `FileVersion` and `ProductVersion`, where nothing keys a setting
//! off it.
//!
//! [`Resource::new`] takes the description separately from the version for
//! exactly this reason: the two cannot be concatenated by accident.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What to embed.
pub struct Resource {
    /// `(ordinal, path)`. **Ordinal 1 is the application icon** — the one
    /// Explorer, the taskbar and `Icon::from_resource(1, ..)` read — so
    /// whatever should represent the program goes first.
    pub icons: Vec<(u16, PathBuf)>,
    /// The name Windows shows. **Never carries a version.** See the module
    /// documentation for what breaks when it does.
    pub file_description: String,
    pub product_name: String,
    pub internal_name: String,
    pub original_filename: String,
    /// The full version, prerelease suffix and all. Goes in the string values
    /// only; `FILEVERSION` takes the numeric part.
    pub version: String,
    /// The release's build number. `FILEVERSION` is four 16-bit numbers and
    /// nothing else — so a prerelease suffix cannot travel in it, and its
    /// fourth field is [`build_field`] of this.
    pub build_number: u64,
    /// Identity of the sparse MSIX package that grants this executable a
    /// stable notification identity. Absent for ordinary developer builds.
    pub package_identity: Option<PackageIdentity>,
}

/// The package identity shared by the Agent executable and its sparse MSIX.
///
/// `name`, `application_id`, and `toast_activator_clsid` are deliberately
/// channel-specific: stable, beta, nightly, and local builds can coexist
/// without stealing one another's toast activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentity {
    pub name: &'static str,
    pub application_id: &'static str,
    pub toast_activator_clsid: &'static str,
    pub publisher: String,
}

/// Return the immutable Windows notification identity for a release segment.
pub fn notification_identity(segment: &str, publisher: impl Into<String>) -> PackageIdentity {
    let (name, toast_activator_clsid) = match segment {
        "stable" => ("Koru.Desktop", "B9A3A8E8-375F-4D89-AC62-4D61A93F2901"),
        "beta" => ("Koru.Desktop.Beta", "1C63D693-1E45-42CF-98F9-D26C590E1C72"),
        "nightly" => (
            "Koru.Desktop.Nightly",
            "78506633-9710-428D-9305-2C5453EFC7D7",
        ),
        "internal-debug" => (
            "Koru.Desktop.InternalDebug",
            "7293414C-D228-4A20-BB18-5B9B48F7CD9F",
        ),
        "internal-release" => (
            "Koru.Desktop.InternalRelease",
            "48FB1370-B239-4189-A38C-85F6C784935F",
        ),
        other => panic!("unknown Koru release segment {other}"),
    };
    PackageIdentity {
        name,
        application_id: "KoruAgent",
        toast_activator_clsid,
        publisher: publisher.into(),
    }
}

impl Resource {
    /// A resource with one icon at ordinal 1.
    pub fn new(name: &str, exe: &str, icon: impl Into<PathBuf>) -> Self {
        Self {
            icons: vec![(1, icon.into())],
            file_description: name.to_string(),
            product_name: name.to_string(),
            internal_name: exe.trim_end_matches(".exe").to_string(),
            original_filename: exe.to_string(),
            version: env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()),
            build_number: 0,
            package_identity: None,
        }
    }

    /// Add another icon at `ordinal`. Ordinal 1 stays the application icon.
    pub fn icon(mut self, ordinal: u16, path: impl Into<PathBuf>) -> Self {
        self.icons.push((ordinal, path.into()));
        self
    }

    /// Override the version, from `KORU_VERSION` and `KORU_BUILD_NUMBER` where
    /// a release sets them.
    pub fn versioned(mut self, version: String, build_number: u64) -> Self {
        self.version = version;
        self.build_number = build_number;
        self
    }

    /// Attach the sparse-package identity to the executable manifest.
    pub fn package_identity(mut self, identity: PackageIdentity) -> Self {
        self.package_identity = Some(identity);
        self
    }
}

/// The application icon for the channel this build is producing — the tinted
/// mark, so a nightly tray icon looks nightly ([ADR — channel tints]).
///
/// Reads the **live** environment, which a build script may and `option_env!`
/// in `koru-channel` may not: `KORU_RELEASE_CHANNEL` is what `koru-release`
/// sets (absent means internal, same default as `build_info`), and `PROFILE`
/// is cargo's debug/release split, mirroring `segment_of`'s five segments.
/// Stable keeps the untinted `koru.ico`; the others name the `koru-<segment>.ico`
/// variants that `scripts/icons/set-logo.py` emits.
pub fn channel_icon(assets: &Path) -> PathBuf {
    channel_named_icon(assets, "koru")
}

/// The channel-specific icon for a Koru executable with its own icon family,
/// such as `koru-ai-chat[-<segment>].ico`.
pub fn channel_named_icon(assets: &Path, stem: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed=KORU_RELEASE_CHANNEL");
    let channel = env::var("KORU_RELEASE_CHANNEL").unwrap_or_else(|_| "internal".into());
    let channel = koru_channel::ReleaseChannel::from_build_channel(&channel);
    let debug = env::var("PROFILE").as_deref() == Ok("debug");
    match koru_channel::segment_of(channel, debug) {
        "stable" => assets.join(format!("{stem}.ico")),
        segment => assets.join(format!("{stem}-{segment}.ico")),
    }
}

/// Compile `resource` and link it into `bin_name`.
///
/// A no-op off Windows, so a build script can call it unconditionally.
/// Panics on a real failure: a binary that silently lost its icon looks
/// correct on the machine that built it and wrong everywhere else.
pub fn embed(bin_name: &str, resource: &Resource) {
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WINDRES");
    for (_, icon) in &resource.icons {
        println!("cargo:rerun-if-changed={}", icon.display());
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output"));
    let script = write_script(&out_dir, bin_name, resource);

    let compiled = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        compile_with_rc(&manifest, &out_dir, bin_name, &script)
    } else {
        compile_with_windres(&manifest, &out_dir, bin_name, &script)
    };
    println!("cargo:rustc-link-arg-bin={bin_name}={}", compiled.display());
}

/// The `major.minor.patch` of a semantic version, ignoring any prerelease or
/// build metadata. `FILEVERSION` is four numbers and will take nothing else.
pub fn numeric_core(version: &str) -> (u32, u32, u32) {
    let core = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .trim_start_matches('v');
    let mut parts = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// **The fourth field of a Windows version for a build number**, which Windows
/// holds in 16 bits — in `FILEVERSION` and in the sparse MSIX's `Version`,
/// where it must also grow from one build to the next or the package is not
/// re-registered.
///
/// A number the update origin allocated is small and is the field as it is. A
/// GitHub nightly's is its UTC build time, `yyyymmddHHMM` (NIGHTLY N11), which
/// does not fit; it becomes the hours since 2026-01-01 00:00 UTC — growing,
/// and room until mid-2033. Two nightlies within one hour share a field.
/// `None` is any other number too large to hold: refused, never wrapped,
/// because a wrapped field is a package Windows thinks is older.
pub fn build_field(build_number: u64) -> Option<u16> {
    if let Ok(field) = u16::try_from(build_number) {
        return Some(field);
    }
    let hours = build_time_hours(build_number)?;
    u16::try_from(hours.checked_sub(BUILD_FIELD_EPOCH_HOURS)?).ok()
}

/// 2026-01-01 00:00 UTC, in hours since the Unix epoch.
const BUILD_FIELD_EPOCH_HOURS: i64 = 20_454 * 24;

/// `yyyymmddHHMM` as hours since the Unix epoch, or `None` if it is not a
/// real UTC minute.
fn build_time_hours(build_number: u64) -> Option<i64> {
    let text = build_number.to_string();
    if text.len() != 12 {
        return None;
    }
    let field = |range: std::ops::Range<usize>| text[range].parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute) = (field(8..10)?, field(10..12)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || !(1..=month_days[month as usize - 1]).contains(&day)
        || hour > 23
        || minute > 59
    {
        return None;
    }
    // Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some((era * 146_097 + day_of_era - 719_468) * 24 + hour)
}

/// The sparse MSIX's four-part `Version` for a release, which the packager
/// writes and the Agent compares its registration against — one function, so
/// the two cannot disagree. Every field is 16 bits.
pub fn package_version(version: &str, build_number: u64) -> Result<String, String> {
    let (major, minor, patch) = numeric_core(version);
    let field = build_field(build_number).ok_or_else(|| {
        format!(
            "build number {build_number} has no Windows version field: it is neither at most {} \
             nor a yyyymmddHHMM build time from 2026 on",
            u16::MAX
        )
    })?;
    if [major, minor, patch]
        .iter()
        .any(|part| *part > u16::MAX as u32)
    {
        return Err(format!(
            "Windows version fields must be at most {}; got {version}",
            u16::MAX
        ));
    }
    Ok(format!("{major}.{minor}.{patch}.{field}"))
}

fn write_script(out_dir: &Path, bin_name: &str, resource: &Resource) -> PathBuf {
    let icons: String = resource
        .icons
        .iter()
        .map(|(ordinal, path)| {
            // rc.exe reads a backslash inside a quoted resource path as a C
            // escape, so every path is written with forward slashes, which it
            // accepts on every version.
            let path = path.display().to_string().replace('\\', "/");
            format!("{ordinal} ICON \"{path}\"\n")
        })
        .collect();

    let (major, minor, patch) = numeric_core(&resource.version);
    // TaskDialog and the modern common controls live in the version-6 side by
    // side assembly. Without this manifest Windows loads the legacy comctl32
    // surface, which has no TaskDialogIndirect at all.
    let manifest = out_dir.join(format!("{bin_name}.manifest"));
    std::fs::write(&manifest, assembly_manifest(resource))
        .expect("write Common Controls and package identity manifest");
    let manifest = manifest.display().to_string().replace('\\', "/");
    let Resource {
        file_description,
        product_name,
        internal_name,
        original_filename,
        version,
        build_number,
        ..
    } = resource;
    // Refused rather than wrapped or zeroed: the build number is what a
    // release is, and a binary that lost it looks right only where it was built.
    let build_number = build_field(*build_number).unwrap_or_else(|| {
        panic!("KORU_BUILD_NUMBER {build_number} does not fit a Windows version field")
    });

    let script = format!(
        r#"#include <windows.h>

/* Generated by koru-winres from a build script -- edit that, not this. */

{icons}
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},{build_number}
PRODUCTVERSION {major},{minor},{patch},{build_number}
FILEFLAGSMASK VS_FFI_FILEFLAGSMASK
FILEFLAGS 0x0L
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "Koru\0"
            VALUE "FileDescription", "{file_description}\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "{internal_name}\0"
            VALUE "OriginalFilename", "{original_filename}\0"
            VALUE "ProductName", "{product_name}\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    let path = out_dir.join(format!("{bin_name}.rc"));
    std::fs::write(&path, script).expect("write the generated resource script");
    path
}

fn assembly_manifest(resource: &Resource) -> String {
    let package_identity = resource
        .package_identity
        .as_ref()
        .map_or_else(String::new, |identity| {
            format!(
                "\n  <msix xmlns=\"urn:schemas-microsoft-com:msix.v1\" publisher=\"{}\" packageName=\"{}\" applicationId=\"{}\"/>",
                xml_attribute(&identity.publisher), identity.name, identity.application_id
            )
        });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>{package_identity}
</assembly>"#
    )
}

fn xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn compile_with_rc(manifest: &Path, out_dir: &Path, bin_name: &str, script: &Path) -> PathBuf {
    let output = out_dir.join(format!("{bin_name}.res"));
    let mut cmd = match env::var_os("RC") {
        // An explicit override is trusted to run in an environment (a developer
        // prompt) where INCLUDE is already set.
        Some(rc) => Command::new(rc),
        None => {
            let (rc, include) = find_rc().expect(
                "no resource compiler: install a Windows 10/11 SDK, \
                 or set RC to an rc.exe and run from a VS developer prompt",
            );
            let mut cmd = Command::new(rc);
            cmd.arg(format!("/I{}", include.join("um").display()));
            cmd.arg(format!("/I{}", include.join("shared").display()));
            cmd
        }
    };
    let status = cmd
        .current_dir(manifest)
        .arg("/nologo")
        .arg("/fo")
        .arg(&output)
        .arg(script)
        .status()
        .unwrap_or_else(|e| panic!("could not run rc.exe to embed {bin_name}'s icon: {e}"));
    assert!(status.success(), "Windows resource compiler failed");
    output
}

/// The newest installed SDK that has **both** `rc.exe` for this host and its
/// matching `Include` tree.
///
/// The two are versioned together, and one without the other fails at resource
/// time rather than here — with a complaint about `winver.h` that says nothing
/// about SDKs and sends people looking in the wrong place.
pub fn find_rc() -> Option<(PathBuf, PathBuf)> {
    let root = PathBuf::from(env::var_os("ProgramFiles(x86)")?)
        .join("Windows Kits")
        .join("10");
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "x86",
    };
    // Sorted as numbers, not as strings: `10.0.22621.0` must beat `10.0.9.0`,
    // and lexicographic order gets that backwards.
    let mut versions: Vec<(Vec<u32>, String)> = std::fs::read_dir(root.join("bin"))
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let nums = name
                .split('.')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
                .ok()?;
            Some((nums, name))
        })
        .collect();
    versions.sort();
    versions.iter().rev().find_map(|(_, name)| {
        let rc = root.join("bin").join(name).join(arch).join("rc.exe");
        let include = root.join("Include").join(name);
        (rc.is_file() && include.join("um").join("winver.h").is_file()).then_some((rc, include))
    })
}

fn compile_with_windres(manifest: &Path, out_dir: &Path, bin_name: &str, script: &Path) -> PathBuf {
    let output = out_dir.join(format!("{bin_name}-res.o"));
    let compiler = env::var_os("WINDRES").unwrap_or_else(|| "windres.exe".into());
    let status = Command::new(&compiler)
        .current_dir(manifest)
        .arg("--input")
        .arg(script)
        .arg("--output")
        .arg(&output)
        .arg("--output-format=coff")
        .status()
        .unwrap_or_else(|e| panic!("could not run {compiler:?} to embed {bin_name}'s icon: {e}"));
    assert!(status.success(), "Windows resource compiler failed");
    output
}

#[cfg(test)]
mod tests;
