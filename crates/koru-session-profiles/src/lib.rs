// SPDX-License-Identifier: MPL-2.0

//! Shared raw-session launch definitions and managed registration data.
//!
//! The session host is the launch authority and Koru Terminal presents the
//! same rows, so executable argv and attachment-delivery policy must have one
//! owner. This crate performs no spawning, root discovery, persistence, UI, or
//! localization. The Codex structured adapter is a separate compatibility
//! gate; this catalogue intentionally retains Codex's always-available raw
//! fallback rather than treating protocol support as executable presence.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

mod managed;
pub use managed::*;

use koru_session_protocol::{
    AttachmentDeliveryPolicy, BuiltInProfileKey, ErrorCode, ExecutableAvailability, ProfileKind,
    ProviderKind, SessionProfileId, SessionProfileKey, ShellQuoting, built_in_profile,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileLabel {
    PlatformShell,
    PowerShell,
    Bash,
    Claude,
    OpenCode,
    CodexEmbedded,
    CodexCli,
    Grok,
    Kimi,
    Copilot,
    Gemini,
    QwenCode,
    Antigravity,
    Muse,
    Custom,
}

impl ProfileLabel {
    /// Content-neutral fallback for non-UI callers such as the public API.
    /// Product surfaces localize the enum rather than persisting this string.
    pub const fn fallback(self) -> &'static str {
        match self {
            Self::PlatformShell => "Platform shell",
            Self::PowerShell => "PowerShell",
            Self::Bash => "Bash",
            Self::Claude => "Claude",
            Self::OpenCode => "OpenCode",
            Self::CodexEmbedded => "Codex (Embedded)",
            Self::CodexCli => "Codex CLI",
            Self::Grok => "Grok",
            Self::Kimi => "Kimi",
            Self::Copilot => "GitHub Copilot",
            Self::Gemini => "Gemini",
            Self::QwenCode => "Qwen Code",
            Self::Antigravity => "Antigravity",
            Self::Muse => "Muse",
            Self::Custom => "Custom profile",
        }
    }

    /// The name product surfaces render for this label when it is a **brand
    /// name** — identical in every language, so it lives here as the one
    /// mapping both launch surfaces (Koru Terminal's menus and koru.exe's `+`
    /// menu) draw from, rather than as two copies that drift.
    ///
    /// **"Codex", not "Codex CLI"** (owner, 2026-09-02). The pair reads
    /// "Codex" and "Codex (Embedded)", so the plain row is the plain program
    /// and the parenthesis is the one that needs explaining — which is the way
    /// round it was not. Copy only: the wire name, the id and the
    /// `built_in_profile` key are frozen (ADR-263), and the variant is still
    /// `CodexCli` because that is what the protocol calls it.
    ///
    /// `None` for the two labels whose display name is an ordinary word a
    /// surface must localize itself — the platform shell and an unnamed custom
    /// profile. This crate holds no localization (see the module header), so
    /// those two words stay with the surfaces and arrive through
    /// [`display_name`]'s arguments.
    pub const fn brand_name(self) -> Option<&'static str> {
        match self {
            Self::PowerShell => Some("PowerShell"),
            Self::Bash => Some("Bash"),
            Self::Claude => Some("Claude"),
            Self::OpenCode => Some("OpenCode"),
            Self::CodexEmbedded => Some("Codex (Embedded)"),
            Self::CodexCli => Some("Codex"),
            Self::Grok => Some("Grok"),
            Self::Kimi => Some("Kimi"),
            // **"GitHub Copilot" and "Qwen Code", in full.** Both are the
            // product's own name; "Copilot" alone is Microsoft's assistant in
            // half a dozen other places, and "Qwen" alone is the model rather
            // than the CLI.
            Self::Copilot => Some("GitHub Copilot"),
            Self::Gemini => Some("Gemini"),
            Self::QwenCode => Some("Qwen Code"),
            Self::Antigravity => Some("Antigravity"),
            Self::Muse => Some("Muse"),
            Self::PlatformShell | Self::Custom => None,
        }
    }
}

/// The display name a launch surface renders for `profile` — the single
/// projection every `+` menu calls, so two surfaces cannot disagree about
/// what a profile is called (koru.exe's menu used to say "Codex CLI" where
/// Koru Terminal's said "Codex").
///
/// An authored custom name wins; a built-in brand name comes from
/// [`ProfileLabel::brand_name`]; the two remaining labels are ordinary words
/// the caller passes in its own language — `platform_shell` and `custom` —
/// because this crate deliberately owns no translatable strings.
pub fn display_name<'a>(
    profile: &'a LaunchProfile,
    platform_shell: &'a str,
    custom: &'a str,
) -> &'a str {
    profile.authored_name().unwrap_or(match profile.label {
        ProfileLabel::PlatformShell => platform_shell,
        ProfileLabel::Custom => custom,
        label => match label.brand_name() {
            Some(brand) => brand,
            // Unreachable today — every other label is a brand — and kept as
            // a fallback rather than a panic so a future variant added to the
            // `None` arm above reads wrong instead of crashing a draw.
            None => label.fallback(),
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchProfile {
    pub id: SessionProfileKey,
    pub label: ProfileLabel,
    custom_name: Option<String>,
    pub kind: ProfileKind,
    pub argv: Vec<String>,
    pub provider: Option<ProviderKind>,
    pub attachment_delivery: AttachmentDeliveryPolicy,
}

impl LaunchProfile {
    pub fn authored_name(&self) -> Option<&str> {
        self.custom_name.as_deref()
    }

    pub fn fallback_display_name(&self) -> &str {
        self.authored_name()
            .unwrap_or_else(|| self.label.fallback())
    }

    pub fn custom(id: SessionProfileId, name: impl Into<String>, argv: Vec<String>) -> Self {
        Self {
            id: SessionProfileKey::Custom(id),
            label: ProfileLabel::Custom,
            custom_name: Some(name.into()),
            kind: ProfileKind::Custom,
            argv,
            provider: None,
            attachment_delivery: AttachmentDeliveryPolicy::Unsupported,
        }
    }
}

/// The platform shell's argv — the program, and on macOS the one flag that
/// makes it usable.
///
/// **A macOS app launched from Finder does not have the user's `PATH`.**
/// launchd hands a GUI process `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and
/// nothing else; the real path is assembled by `path_helper` from `/etc/paths`
/// and `/etc/paths.d`, which runs in `/etc/zprofile` — a *login* shell's file.
/// A non-login `zsh` reads `~/.zshrc` and skips all of that, so a session
/// spawned by a bundled Koru inherits the four launchd directories and
/// `brew`, `node`, `cargo`, `claude` and `opencode` are all "command not
/// found". The Terminal that ships with macOS spawns a login shell for exactly
/// this reason, and so does every editor with an embedded one.
///
/// **Windows and Linux are deliberately unchanged.** Windows has no such
/// concept, and a Linux desktop session already exports the login environment
/// to everything it starts — where re-sourcing the profile per terminal is a
/// behaviour change users notice and did not ask for.
fn platform_shell_argv() -> Vec<String> {
    if cfg!(windows) {
        return vec![std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())];
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    if cfg!(target_os = "macos") {
        vec![shell, "-l".to_owned()]
    } else {
        vec![shell]
    }
}

pub fn built_in_profiles() -> Vec<LaunchProfile> {
    [
        raw(
            built_in_profile::PLATFORM_SHELL,
            ProfileLabel::PlatformShell,
            ProfileKind::Shell,
            platform_shell_argv(),
            None,
            if cfg!(windows) {
                AttachmentDeliveryPolicy::Unsupported
            } else {
                AttachmentDeliveryPolicy::QuotedPath {
                    shell: ShellQuoting::Posix,
                }
            },
        ),
        raw(
            built_in_profile::POWERSHELL,
            ProfileLabel::PowerShell,
            ProfileKind::Shell,
            vec![
                if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "pwsh"
                }
                .into(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NoExit".into(),
            ],
            None,
            AttachmentDeliveryPolicy::QuotedPath {
                shell: ShellQuoting::PowerShell,
            },
        ),
        raw(
            built_in_profile::BASH,
            ProfileLabel::Bash,
            ProfileKind::Shell,
            vec!["bash".into()],
            None,
            AttachmentDeliveryPolicy::QuotedPath {
                shell: ShellQuoting::Posix,
            },
        ),
        raw_agent(built_in_profile::CLAUDE, ProfileLabel::Claude, "claude"),
        raw_agent(
            built_in_profile::OPENCODE,
            ProfileLabel::OpenCode,
            "opencode",
        ),
        raw_agent(
            built_in_profile::CODEX,
            ProfileLabel::CodexEmbedded,
            "codex",
        ),
        raw_agent(built_in_profile::GROK, ProfileLabel::Grok, "grok"),
        raw_agent(built_in_profile::KIMI, ProfileLabel::Kimi, "kimi"),
        raw(
            built_in_profile::CODEX_CLI,
            ProfileLabel::CodexCli,
            ProfileKind::Agent,
            vec!["codex".into()],
            None,
            AttachmentDeliveryPolicy::Unsupported,
        ),
        // Active entries keep `built_in_profile::ALL`'s order. Retired keys stay
        // reserved in that list but no longer resolve to a launch profile.
        //
        // The programs are the names these CLIs install themselves under, and
        // each row appears only where its executable resolves — see
        // `ProfileRow::availability`, which is what makes a catalogue entry
        // safe to add for a tool most machines do not have.
        raw_agent(built_in_profile::COPILOT, ProfileLabel::Copilot, "copilot"),
        raw_agent(built_in_profile::QWEN, ProfileLabel::QwenCode, "qwen"),
        // Antigravity installs itself as `agy` (AGENTUX I9, D1-8). The tab
        // stays raw; a chat's managed session is Google's separate ACP server,
        // `ANTIGRAVITY_REGISTRATION`.
        raw_agent(
            built_in_profile::ANTIGRAVITY,
            ProfileLabel::Antigravity,
            "agy",
        ),
        // Meta's Muse Code installs itself as `muse` (a `muse.cmd` launcher on
        // Windows). Raw only: its structured `muse serve` (MSP) has no adapter.
        raw_agent(built_in_profile::MUSE, ProfileLabel::Muse, "muse"),
    ]
    .into_iter()
    .collect()
}

pub fn resolve(key: &SessionProfileKey) -> Result<LaunchProfile, ErrorCode> {
    built_in_profiles()
        .into_iter()
        .find(|profile| &profile.id == key)
        .ok_or(ErrorCode::ProfileNotFound)
}

fn raw_agent(key: &str, label: ProfileLabel, program: &str) -> LaunchProfile {
    raw(
        key,
        label,
        ProfileKind::Agent,
        vec![program.to_owned()],
        Some(ProviderKind::from_str(key).expect("built-in provider token")),
        AttachmentDeliveryPolicy::Unsupported,
    )
}

fn raw(
    key: &str,
    label: ProfileLabel,
    kind: ProfileKind,
    argv: Vec<String>,
    provider: Option<ProviderKind>,
    attachment_delivery: AttachmentDeliveryPolicy,
) -> LaunchProfile {
    LaunchProfile {
        id: SessionProfileKey::BuiltIn(
            BuiltInProfileKey::from_str(key).expect("built-in profile token"),
        ),
        label,
        custom_name: None,
        kind,
        argv,
        provider,
        attachment_delivery,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRow {
    pub profile: LaunchProfile,
    pub availability: ExecutableAvailability,
}

impl ProfileRow {
    pub fn launchable(&self) -> bool {
        matches!(self.availability, ExecutableAvailability::Available { .. })
    }
}

/// Presence only. Structured compatibility belongs to provider discovery.
pub fn probe(profiles: Vec<LaunchProfile>) -> Vec<ProfileRow> {
    profiles
        .into_iter()
        .map(|profile| {
            let available = resolve_launch_argv(&profile.argv).is_some();
            ProfileRow {
                profile,
                availability: if available {
                    ExecutableAvailability::Available { version: None }
                } else {
                    ExecutableAvailability::Missing
                },
            }
        })
        .collect()
}

/// Resolve the same concrete executable candidate used by presence probing.
/// The session host calls this only after selecting an authored profile; it is
/// not root discovery and never searches for user content.
pub fn resolve_executable(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file().then(|| absolute(path)).flatten();
    }
    let path_value = std::env::var_os("PATH")?;
    let directories = std::env::split_paths(&path_value).collect::<Vec<_>>();
    #[cfg(windows)]
    if program.eq_ignore_ascii_case("bash") {
        // `C:\Windows\System32\bash.exe` is the legacy WSL launcher, not a
        // standalone Bash. It can be present while no distribution exists,
        // which made the row look available and then exit immediately. Koru's
        // Windows Bash profile is the native Git Bash PTY instead.
        if let Some(candidate) = preferred_windows_bash(&directories) {
            return absolute(&candidate);
        }
        return None;
    }
    for directory in directories {
        for name in executable_candidates(program) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return absolute(&candidate);
            }
        }
    }
    None
}

/// The arguments that make a raw agent continue its **most recent
/// conversation in the folder it starts in**, or `None` for a profile whose
/// CLI has no such form (a shell, Muse, a custom profile). Each was read from
/// the CLI's own `--help` on 2026-10-28.
///
/// This is the whole of what *Resume* means: Koru does not know the agent's
/// session id (a raw PTY never tells it), so it asks the agent for "the last
/// one here". The host appends these after the profile's own argv, which is
/// why the client sends a flag and never a command line.
pub fn resume_arguments(label: ProfileLabel) -> Option<&'static [&'static str]> {
    match label {
        ProfileLabel::Claude
        | ProfileLabel::OpenCode
        | ProfileLabel::QwenCode
        | ProfileLabel::Copilot
        | ProfileLabel::Kimi
        | ProfileLabel::Grok
        | ProfileLabel::Antigravity => Some(&["--continue"]),
        // Codex's continue is a subcommand, not a flag.
        ProfileLabel::CodexEmbedded | ProfileLabel::CodexCli => Some(&["resume", "--last"]),
        ProfileLabel::PlatformShell
        | ProfileLabel::PowerShell
        | ProfileLabel::Bash
        | ProfileLabel::Gemini
        | ProfileLabel::Muse
        | ProfileLabel::Custom => None,
    }
}

/// Resolve an authored profile argv to the concrete invocation the host will
/// spawn. Presence probing and process launch therefore cannot disagree about
/// which Windows shim was selected.
pub fn resolve_launch_argv(argv: &[String]) -> Option<Vec<OsString>> {
    let (program, arguments) = argv.split_first()?;
    let executable = resolve_executable(program)?;
    platform_launch_argv(executable, arguments)
}

/// Wire-compatible form of the concrete launch argv. This exists only for a
/// newer Terminal talking to an older host whose built-in profile catalogue
/// does not yet contain an append-only key. Normal built-in launches remain
/// host-resolved; a path that cannot be represented losslessly on the wire is
/// not weakened with lossy conversion.
pub fn resolve_launch_strings(argv: &[String]) -> Option<Vec<String>> {
    resolve_launch_argv(argv)?
        .into_iter()
        .map(|argument| argument.into_string().ok())
        .collect()
}

fn absolute(path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok().or_else(|| {
        path.is_absolute()
            .then(|| path.to_path_buf())
            .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(path)))
    })?;
    #[cfg(windows)]
    return Some(windows_compatible_path(&path));
    #[cfg(not(windows))]
    Some(path)
}

fn executable_candidates(program: &str) -> impl Iterator<Item = PathBuf> {
    let mut names = Vec::new();
    #[cfg(windows)]
    if Path::new(program).extension().is_none() {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        names.extend(
            extensions
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(|extension| PathBuf::from(format!("{program}{extension}"))),
        );
    } else {
        names.push(PathBuf::from(program));
    }
    #[cfg(not(windows))]
    names.push(PathBuf::from(program));
    names.into_iter()
}

#[cfg(windows)]
fn preferred_windows_bash(directories: &[PathBuf]) -> Option<PathBuf> {
    let installed = [
        std::env::var_os("ProgramW6432"),
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
    ]
    .into_iter()
    .flatten()
    .flat_map(|root| {
        let root = PathBuf::from(root).join("Git");
        [root.join("bin/bash.exe"), root.join("usr/bin/bash.exe")]
    })
    .find(|candidate| candidate.is_file());
    installed.or_else(|| {
        directories
            .iter()
            .filter(|directory| {
                directory.components().any(|part| {
                    part.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("git")
                })
            })
            .map(|directory| directory.join("bash.exe"))
            .find(|candidate| candidate.is_file())
    })
}

/// Whether cmd.exe hands `argument` to a batch file's `%*` exactly as it was
/// given. Its operators (`& | < > ^`), both expansion sigils (`% !`), the
/// quote itself and line breaks are refused. Whitespace is fine: the spawner
/// quotes such an argument, and cmd.exe keeps those quotes for the batch file
/// to pass on to the program it wraps. Platform-neutral, so the rule is
/// tested everywhere even though only Windows applies it.
pub fn cmd_transparent(argument: &str) -> bool {
    !argument.is_empty()
        && !argument.chars().any(|character| {
            matches!(
                character,
                '&' | '|' | '<' | '>' | '^' | '%' | '!' | '"' | '\n' | '\r'
            )
        })
}

/// Render an already-resolved executable using the same platform boundary as raw
/// profiles. A Windows batch shim takes an authored argument only when it is
/// [`cmd_transparent`]; otherwise the launch is refused.
pub fn platform_launch_argv(executable: PathBuf, arguments: &[String]) -> Option<Vec<OsString>> {
    #[cfg(windows)]
    let executable = windows_compatible_path(&executable);
    #[cfg(windows)]
    if executable
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
    {
        // cmd.exe reparses everything after /c, so ordinary argv quoting does
        // not make its metacharacters safe for that second parser. An
        // argument rides through only when it is transparent to cmd.exe —
        // the flag and the path a tab's MCP bridge needs are (`tab_bridge`
        // in `koru-session-host`); anything else is refused, never reparsed.
        if !arguments.iter().all(|argument| cmd_transparent(argument)) {
            return None;
        }
        let mut launch = vec![
            std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe")),
            OsString::from("/d"),
            OsString::from("/s"),
            OsString::from("/c"),
            OsString::from("call"),
            executable.into_os_string(),
        ];
        launch.extend(arguments.iter().map(OsString::from));
        return Some(launch);
    }
    let mut launch = Vec::with_capacity(arguments.len() + 1);
    launch.push(executable.into_os_string());
    launch.extend(arguments.iter().map(OsString::from));
    Some(launch)
}

#[cfg(windows)]
fn windows_compatible_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    if !units.starts_with(&verbatim) {
        return path.to_path_buf();
    }
    let unc = [b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
    if units[verbatim.len()..].starts_with(&unc) {
        let mut ordinary = vec![b'\\' as u16, b'\\' as u16];
        ordinary.extend_from_slice(&units[verbatim.len() + unc.len()..]);
        PathBuf::from(OsString::from_wide(&ordinary))
    } else {
        PathBuf::from(OsString::from_wide(&units[verbatim.len()..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_are_complete_stable_and_resolvable() {
        let profiles = built_in_profiles();
        let active_keys: Vec<_> = built_in_profile::ALL
            .iter()
            .copied()
            .filter(|key| *key != built_in_profile::GEMINI)
            .collect();
        assert_eq!(profiles.len(), active_keys.len());
        let retired = SessionProfileKey::BuiltIn(built_in_profile::GEMINI.parse().unwrap());
        assert_eq!(resolve(&retired), Err(ErrorCode::ProfileNotFound));
        let keys: Vec<_> = profiles
            .iter()
            .map(|profile| match &profile.id {
                SessionProfileKey::BuiltIn(key) => key.as_str(),
                SessionProfileKey::Custom(_) => panic!("built-in became custom"),
            })
            .collect();
        assert_eq!(keys, active_keys);
        assert!(profiles.iter().all(|profile| !profile.argv.is_empty()));
        for profile in profiles {
            assert_eq!(resolve(&profile.id), Ok(profile));
        }
    }

    /// **The macOS platform shell is a login shell, and nothing else's is.**
    /// Without `-l` a bundled Koru's terminal inherits launchd's four-entry
    /// `PATH` and cannot find anything the user installed — see
    /// [`platform_shell_argv`]. Asserted here rather than left to a reviewer
    /// because the flag looks like noise and reads as removable.
    #[test]
    fn the_platform_shell_is_a_login_shell_on_macos_only() {
        let argv = platform_shell_argv();
        assert!(!argv.is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(argv.len(), 2);
            assert_eq!(argv[1], "-l");
        } else {
            assert_eq!(argv.len(), 1);
        }
    }

    /// The reconciled display names both launch surfaces render. The one that
    /// earns the pin: the Codex CLI profile reads **"Codex"**, never the wire
    /// fallback "Codex CLI" (owner, 2026-09-02) — and the localizable pair
    /// passes through the caller's words untouched.
    #[test]
    fn display_names_are_reconciled_and_brand_names_are_shared() {
        let by_label = |label: ProfileLabel| {
            built_in_profiles()
                .into_iter()
                .find(|profile| profile.label == label)
                .expect("a built-in carries every non-custom label")
        };
        assert_eq!(
            display_name(&by_label(ProfileLabel::CodexCli), "System", "Custom"),
            "Codex"
        );
        assert_eq!(
            display_name(&by_label(ProfileLabel::CodexEmbedded), "System", "Custom"),
            "Codex (Embedded)"
        );
        assert_eq!(
            display_name(&by_label(ProfileLabel::PlatformShell), "System", "Custom"),
            "System",
            "the platform shell renders the caller's localized word"
        );
        let authored = LaunchProfile::custom(SessionProfileId::mint(), "My REPL", vec!["r".into()]);
        assert_eq!(
            display_name(&authored, "System", "Custom"),
            "My REPL",
            "an authored name outranks every mapping"
        );
        // Every built-in resolves to a non-empty name without touching the
        // content-neutral fallback's "Codex CLI".
        for profile in built_in_profiles() {
            let name = display_name(&profile, "System", "Custom");
            assert!(!name.is_empty());
            assert_ne!(name, "Codex CLI");
        }
    }

    #[test]
    fn custom_profiles_require_the_future_authored_catalog() {
        assert_eq!(
            resolve(&SessionProfileKey::Custom(SessionProfileId::mint())),
            Err(ErrorCode::ProfileNotFound)
        );
    }

    #[test]
    fn missing_programs_are_disabled_without_a_spawn() {
        let profile = raw(
            built_in_profile::BASH,
            ProfileLabel::Bash,
            ProfileKind::Shell,
            vec!["koru-a-program-that-does-not-exist-8f3b".into()],
            None,
            AttachmentDeliveryPolicy::Unsupported,
        );
        let rows = probe(vec![profile]);
        assert_eq!(rows[0].availability, ExecutableAvailability::Missing);
        assert!(!rows[0].launchable());
    }

    #[cfg(windows)]
    #[test]
    fn windows_never_selects_an_extensionless_npm_shim_before_pathext() {
        let candidates = executable_candidates("opencode").collect::<Vec<_>>();
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.extension().is_some())
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate == Path::new("opencode"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_profiles_launch_through_the_command_interpreter() {
        let argv =
            platform_launch_argv(PathBuf::from(r"\\?\C:\tools with spaces\opencode.cmd"), &[])
                .unwrap();
        assert_eq!(argv[1..5], ["/d", "/s", "/c", "call"]);
        assert_eq!(
            argv[5],
            OsString::from(r"C:\tools with spaces\opencode.cmd")
        );
        assert_eq!(argv.len(), 6);
        assert!(
            platform_launch_argv(
                PathBuf::from(r"C:\tools\opencode.cmd"),
                &["unsafe & reparsed".into()]
            )
            .is_none()
        );
        // A tab's bridge: a flag and a path, with a space the spawner will
        // quote, ride through after the shim.
        let argv = platform_launch_argv(
            PathBuf::from(r"C:\tools\qwen.cmd"),
            &[
                "--mcp-config".into(),
                r"C:\Users\A B\bridge\qwen.json".into(),
            ],
        )
        .unwrap();
        assert_eq!(argv[4..5], ["call"]);
        assert_eq!(argv[6], OsString::from("--mcp-config"));
        assert_eq!(argv[7], OsString::from(r"C:\Users\A B\bridge\qwen.json"));
        assert_eq!(argv.len(), 8);
    }

    #[test]
    fn cmd_transparency_admits_flags_paths_and_toml_literals_only() {
        for argument in [
            "--mcp-config",
            r"C:\Users\A B\AppData\Local\Koru\agent\sessions\bridge\claude.json",
            r"@C:\Users\me\bridge\copilot.json",
            r"mcp_servers.koru.command='C:\Program Files\Koru\koru-mcp.exe'",
            "mcp_servers.koru.args=['serve','--stdio']",
        ] {
            assert!(cmd_transparent(argument), "{argument}");
        }
        for argument in [
            "",
            "a & b",
            "a|b",
            "<in",
            "out>",
            "^",
            "%PATH%",
            "!x!",
            "\"quoted\"",
            "two\nlines",
        ] {
            assert!(!cmd_transparent(argument), "{argument:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_launch_paths_do_not_expose_the_verbatim_prefix_to_programs() {
        assert_eq!(
            windows_compatible_path(Path::new(r"\\?\C:\Windows\tool.exe")),
            PathBuf::from(r"C:\Windows\tool.exe")
        );
        assert_eq!(
            windows_compatible_path(Path::new(r"\\?\UNC\server\share\tool.exe")),
            PathBuf::from(r"\\server\share\tool.exe")
        );
    }
}
