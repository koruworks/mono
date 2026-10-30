// SPDX-License-Identifier: MPL-2.0

//! **The Windows installer**, `koru-<v>-windows-x86_64-setup.exe`
//! (`docs/features/linux/NIGHTLY.rev2.md` §12, N18 and N19).
//!
//! NSIS, over the same staged directory the zip is made from, so the two hold
//! the same signed executables. The script is checked in at [`SCRIPT`]; this
//! module writes a wrapper beside the stage that defines the values the
//! script needs and includes it, runs `makensis`, and signs the result. The
//! installer is for the first install only: the feed keeps naming the zip,
//! and the installed Koru updates itself from it.

use std::path::{Path, PathBuf};

/// The installer script, relative to the workspace root.
pub(crate) const SCRIPT: &str = "scripts/release/koru-installer.nsi";

/// The installer's name beside the zip, before GitHub's renaming.
pub(crate) fn file_name(version: &str, target: &str) -> String {
    format!("koru-{version}-{target}-setup.exe")
}

/// What a channel's install is called in the Start menu, on the desktop and
/// in Settings -> Apps, and the directory it installs into: one per channel,
/// so channels install side by side.
pub(crate) fn product_name(segment: &str) -> String {
    match segment {
        "stable" => "Koru".into(),
        "beta" => "Koru Beta".into(),
        "nightly" => "Koru Nightly".into(),
        other => format!("Koru ({other})"),
    }
}

/// The channel's icon, as `koru_winres::channel_icon` picks it for the
/// executables.
pub(crate) fn icon(root: &Path, segment: &str) -> PathBuf {
    match segment {
        "stable" => root.join("assets").join("koru.ico"),
        segment => root.join("assets").join(format!("koru-{segment}.ico")),
    }
}

/// Everything the script is told, in the order it documents them.
pub(crate) struct Defines {
    pub product: String,
    pub version: String,
    pub version4: String,
    pub stage: PathBuf,
    pub outfile: PathBuf,
    pub icon: PathBuf,
    pub identity: String,
    pub autostart: String,
    /// `KORU_SIGNTOOL_COMMAND` with NSIS's `%1` for the file; `None` leaves
    /// the uninstaller unsigned, as the rest of an unsigned build is.
    pub uninstall_sign_command: Option<String>,
}

impl Defines {
    pub(crate) fn new(
        root: &Path,
        stage: &Path,
        outfile: &Path,
        version: &str,
        version4: String,
        channel: &str,
        signtool_command: Option<&str>,
    ) -> Self {
        let segment = koru_channel::segment_of(
            koru_channel::ReleaseChannel::from_build_channel(channel),
            false,
        );
        Self {
            product: product_name(segment),
            version: version.into(),
            version4,
            stage: stage.to_path_buf(),
            outfile: outfile.to_path_buf(),
            icon: icon(root, segment),
            // The publisher is irrelevant to the name, which is all the
            // installer uses: the uninstall key, and the package it removes.
            identity: koru_winres::notification_identity(segment, String::new())
                .name
                .into(),
            autostart: koru_autostart::entry_for_segment(koru_autostart::KORU_ENTRY, segment),
            uninstall_sign_command: signtool_command
                .filter(|command| !command.trim().is_empty())
                .map(|command| command.replace("{file}", "\"%1\"")),
        }
    }
}

/// The wrapper `makensis` compiles: the defines, then the script.
///
/// **Every value goes in backquotes, and a value that would need escaping is
/// refused.** NSIS reads `$` as a variable even at compile time and has no
/// escape inside a backquoted string for a backquote, so a path or a signing
/// command holding either would be compiled into something else. Neither
/// appears in a path or a command this pipeline produces; refusing is cheaper
/// than an escaping rule nobody can test.
pub(crate) fn wrapper(defines: &Defines, script: &Path) -> Result<String, String> {
    // **Every path is written with backslashes only.** The paths joined here
    // mix them (`D:\a\mono\scripts/release/...`), and NSIS takes the folder
    // of an `!include` or a `File` wildcard from the last backslash, so it
    // looked for a file named `scripts/release/koru-installer.nsi` and found
    // none — the 2026-10-30 nightly's Windows build failed on exactly that.
    let path = |path: &Path| path.display().to_string().replace('/', "\\");
    let mut values = vec![
        ("PRODUCT", defines.product.clone()),
        ("VERSION", defines.version.clone()),
        ("VERSION4", defines.version4.clone()),
        ("STAGE", path(&defines.stage)),
        ("OUTFILE", path(&defines.outfile)),
        ("ICON", path(&defines.icon)),
        ("IDENTITY", defines.identity.clone()),
        ("AUTOSTART", defines.autostart.clone()),
    ];
    if let Some(command) = &defines.uninstall_sign_command {
        values.push(("UNINSTALL_SIGN_COMMAND", command.clone()));
    }
    let mut text = String::new();
    for (name, value) in values.iter().chain([&("SCRIPT", path(script))]) {
        if value.contains(['$', '`', '\r', '\n']) {
            return Err(format!(
                "the installer's {name} cannot hold `$`, a backquote or a line break: {value}"
            ));
        }
        if *name != "SCRIPT" {
            text.push_str(&format!("!define {name} `{value}`\n"));
        }
    }
    text.push_str(&format!("!include `{}`\n", path(script)));
    Ok(text)
}

/// `makensis`: `MAKENSIS` when it is set, else NSIS's default install
/// location, else whatever `PATH` finds.
#[cfg(windows)]
fn makensis() -> PathBuf {
    if let Some(path) = std::env::var_os("MAKENSIS").filter(|path| !path.is_empty()) {
        return path.into();
    }
    let default = Path::new(r"C:\Program Files (x86)\NSIS\makensis.exe");
    if default.is_file() {
        default.to_path_buf()
    } else {
        "makensis".into()
    }
}

/// Build and sign the installer at `output` from the staged package.
#[cfg(windows)]
pub(crate) fn build(
    root: &Path,
    stage: &Path,
    output: &Path,
    version: &str,
    build_number: u64,
    channel: &str,
) -> Result<(), String> {
    use std::fs;

    let dist = stage
        .parent()
        .ok_or_else(|| "the stage has no parent".to_string())?;
    let signtool = std::env::var("KORU_SIGNTOOL_COMMAND").ok();
    let defines = Defines::new(
        root,
        stage,
        output,
        version,
        crate::package::windows_package_version(version, build_number)?,
        channel,
        signtool.as_deref(),
    );
    let work = dist.join(format!(
        "{}.installer",
        stage
            .file_name()
            .ok_or_else(|| "the stage has no name".to_string())?
            .to_string_lossy()
    ));
    if work.exists() {
        crate::package::ensure_under(dist, &work)?;
        fs::remove_dir_all(&work).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let wrapper_path = work.join("setup.nsi");
    fs::write(&wrapper_path, wrapper(&defines, &root.join(SCRIPT))?)
        .map_err(|error| error.to_string())?;
    match fs::remove_file(output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", output.display())),
    }

    let makensis = makensis();
    let status = std::process::Command::new(&makensis)
        .args(["/V2", "/INPUTCHARSET", "UTF8"])
        .arg(&wrapper_path)
        .status()
        .map_err(|error| {
            format!(
                "could not run {} (NSIS 3.08 or later builds the Windows installer; \
                 set MAKENSIS to its makensis.exe): {error}",
                makensis.display()
            )
        })?;
    if !status.success() {
        return Err(format!("makensis failed for {}", wrapper_path.display()));
    }
    fs::remove_dir_all(&work).map_err(|error| error.to_string())?;
    // The uninstaller inside was signed while it was compiled
    // (`!uninstfinalize`); now the installer around it.
    crate::package::native_sign(output)?;
    println!("[Koru] installer: {}", output.display());
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn build(
    _root: &Path,
    _stage: &Path,
    _output: &Path,
    _version: &str,
    _build_number: u64,
    _channel: &str,
) -> Result<(), String> {
    Err("the Windows installer is built on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nightly(signtool: Option<&str>) -> Defines {
        Defines::new(
            Path::new(r"D:\a\mono"),
            Path::new(r"D:\a\mono\dist\cloud\koru-0.1.4-windows-x86_64"),
            Path::new(r"D:\a\mono\dist\cloud\koru-0.1.4-windows-x86_64-setup.exe"),
            "0.1.4-nightly.20261029+1971586",
            "0.1.4.7239".into(),
            "nightly",
            signtool,
        )
    }

    /// The names the installer writes are the ones the installed Koru uses:
    /// its notification identity and its start-at-sign-in value. The
    /// uninstaller clears both by these names, so a drift leaves them behind.
    #[test]
    fn a_nightly_install_is_named_for_its_channel() {
        let defines = nightly(None);
        assert_eq!(defines.product, "Koru Nightly");
        assert_eq!(defines.identity, "Koru.Desktop.Nightly");
        assert_eq!(defines.autostart, "Koru (nightly)");
        assert!(defines.icon.ends_with("assets/koru-nightly.ico"));
        assert_eq!(product_name("stable"), "Koru");
    }

    #[test]
    fn every_channel_icon_the_installer_names_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for segment in ["stable", "beta", "nightly"] {
            let icon = icon(&root, segment);
            assert!(icon.is_file(), "{}", icon.display());
        }
        assert!(root.join(SCRIPT).is_file());
    }

    /// Unsigned, the uninstaller is left alone; signed, it goes through the
    /// same command as every executable, with NSIS's placeholder for the file.
    #[test]
    fn the_uninstaller_is_signed_exactly_when_the_build_is() {
        let script = Path::new(r"D:\a\mono\scripts\release\koru-installer.nsi");
        let unsigned = wrapper(&nightly(None), script).unwrap();
        assert!(!unsigned.contains("UNINSTALL_SIGN_COMMAND"));
        assert!(unsigned.contains("!define PRODUCT `Koru Nightly`\n"));
        assert!(
            unsigned.ends_with("!include `D:\\a\\mono\\scripts\\release\\koru-installer.nsi`\n")
        );

        let signed = wrapper(
            &nightly(Some(r#""C:\sdk\signtool.exe" sign /fd SHA256 {file}"#)),
            script,
        )
        .unwrap();
        assert!(signed.contains(
            "!define UNINSTALL_SIGN_COMMAND `\"C:\\sdk\\signtool.exe\" sign /fd SHA256 \"%1\"`\n"
        ));
    }

    #[test]
    fn every_path_reaches_nsis_with_backslashes_only() {
        let mut defines = nightly(None);
        defines.stage = PathBuf::from(r"D:\a\mono\dist/cloud\koru-windows-x86_64");
        let text = wrapper(
            &defines,
            Path::new(r"D:\a\mono\scripts/release/koru-installer.nsi"),
        )
        .unwrap();
        assert!(text.contains("!define STAGE `D:\\a\\mono\\dist\\cloud\\koru-windows-x86_64`\n"));
        assert!(text.ends_with("!include `D:\\a\\mono\\scripts\\release\\koru-installer.nsi`\n"));
        assert!(!text.contains('/'));
    }

    #[test]
    fn a_value_nsis_would_read_differently_is_refused() {
        let script = Path::new("koru-installer.nsi");
        let mut defines = nightly(None);
        defines.stage = PathBuf::from(r"C:\$INSTDIR");
        assert!(wrapper(&defines, script).is_err());
        let mut defines = nightly(Some("sign `{file}`"));
        defines.stage = PathBuf::from(r"C:\stage");
        assert!(wrapper(&defines, script).is_err());
    }
}
