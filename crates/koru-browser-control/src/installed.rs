// SPDX-License-Identifier: MPL-2.0

//! Finding the installed browser and reading its version, without starting
//! it.
//!
//! **This is the one file that names a product, and only as data**: the
//! executable's file name, its registered location and its default install
//! folder. Every one of them is "verify at P7.1" (assumption 1).
//!
//! It reads no environment variable. A caller that wants another executable
//! passes its path. Nothing is ever downloaded or bundled.

use std::fmt;
use std::path::{Path, PathBuf};

/// The executable's file name.
const EXECUTABLE_NAME: &str = "msedge.exe";
/// Where an installer registers the executable's full path, under the
/// current user's hive and then the computer's.
const REGISTERED_LOCATION: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe";
/// The default install folder, under each of the computer's program folders.
const DEFAULT_INSTALL_FOLDER: &str = r"Microsoft\Edge\Application";
/// Where Windows records its program folders, and the two values read.
const PROGRAM_FOLDERS_LOCATION: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion";
const PROGRAM_FOLDER_VALUES: [&str; 2] = ["ProgramFilesDir (x86)", "ProgramFilesDir"];

/// The lowest version Koru drives. **Not set**: part A of the qualification
/// measures the version it ran against, and that becomes the floor
/// (assumption 17). Until then no version is refused for being old.
pub const VERSION_FLOOR: Option<BrowserVersion> = None;

/// A four-part product version, ordered part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowserVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub patch: u16,
}

impl BrowserVersion {
    pub const fn new(major: u16, minor: u16, build: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            build,
            patch,
        }
    }

    /// Exactly four decimal parts separated by dots, each at most 65535, as
    /// a version resource holds them. Anything else is not a version.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut next = || {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 5 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u16>().ok()
        };
        let version = Self::new(next()?, next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// The two halves of a file's fixed version resource.
    pub const fn from_fixed_info(most: u32, least: u32) -> Self {
        Self::new(
            (most >> 16) as u16,
            (most & 0xffff) as u16,
            (least >> 16) as u16,
            (least & 0xffff) as u16,
        )
    }
}

impl fmt::Display for BrowserVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.patch
        )
    }
}

/// The installed browser. Its `Debug` shows the version only: an install
/// path under a person's own folder names the person.
#[derive(Clone, PartialEq, Eq)]
pub struct InstalledBrowser {
    pub executable: PathBuf,
    pub version: BrowserVersion,
}

impl fmt::Debug for InstalledBrowser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InstalledBrowser({})", self.version)
    }
}

/// What a probe found. A probe never starts the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Found(InstalledBrowser),
    /// No executable at the passed path, the registered location or a
    /// default install folder.
    NotInstalled,
    /// An executable is there and its version could not be read.
    VersionUnreadable,
    /// The version is below the floor.
    UnsupportedVersion(BrowserVersion),
    /// Not Windows. Nothing is known about any other platform.
    UnsupportedPlatform,
}

/// The paths to look at, in order. With a path from the caller, only that
/// one: a caller that names an executable never gets a different one.
///
/// `registered` is what the registered location holds and `program_folders`
/// are the computer's program folders, both as read and uncleaned.
pub fn candidate_paths(
    explicit: Option<&Path>,
    registered: &[String],
    program_folders: &[String],
) -> Vec<PathBuf> {
    if let Some(path) = explicit {
        return vec![path.to_path_buf()];
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        let seen = paths.iter().any(|known| {
            known
                .to_string_lossy()
                .eq_ignore_ascii_case(&path.to_string_lossy())
        });
        if !seen {
            paths.push(path);
        }
    };
    for value in registered {
        if let Some(path) = registered_path(value) {
            add(path);
        }
    }
    for folder in program_folders {
        let folder = folder.trim().trim_end_matches(['\\', '/']);
        if is_rooted(folder) {
            add(PathBuf::from(format!(
                "{folder}\\{DEFAULT_INSTALL_FOLDER}\\{EXECUTABLE_NAME}"
            )));
        }
    }
    paths
}

/// A registered value as a path: quotes and padding removed, and accepted
/// only when it is a full path to the executable by its own name.
pub fn registered_path(value: &str) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"').trim();
    let name = value.rsplit(['\\', '/']).next()?;
    (is_rooted(value) && name.eq_ignore_ascii_case(EXECUTABLE_NAME)).then(|| PathBuf::from(value))
}

/// `C:\...` or `\\server\...`: decided from the text, so it reads the same
/// on any platform the tests run on.
pub(crate) fn is_rooted(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || path.starts_with(r"\\")
}

/// The version an install folder shows by its layout: the highest of the
/// versioned folders beside the executable. Used only when the executable's
/// own version resource cannot be read.
pub fn version_from_folder_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Option<BrowserVersion> {
    names.into_iter().filter_map(BrowserVersion::parse).max()
}

/// A version against a floor.
pub fn supported(version: BrowserVersion, floor: Option<BrowserVersion>) -> bool {
    floor.is_none_or(|floor| version >= floor)
}

/// What a found executable and the version read for it amount to.
pub fn classify(
    executable: Option<PathBuf>,
    version: Option<BrowserVersion>,
    floor: Option<BrowserVersion>,
) -> Probe {
    let Some(executable) = executable else {
        return Probe::NotInstalled;
    };
    match version {
        None => Probe::VersionUnreadable,
        Some(version) if !supported(version, floor) => Probe::UnsupportedVersion(version),
        Some(version) => Probe::Found(InstalledBrowser {
            executable,
            version,
        }),
    }
}

/// Look for the installed browser, against [`VERSION_FLOOR`].
pub fn probe(explicit: Option<&Path>) -> Probe {
    probe_with_floor(explicit, VERSION_FLOOR)
}

/// Look for the installed browser: the caller's path if it passes one,
/// otherwise the registered location and then the default install folders.
/// Reads the registry and the file's version resource. Starts nothing.
pub fn probe_with_floor(explicit: Option<&Path>, floor: Option<BrowserVersion>) -> Probe {
    #[cfg(windows)]
    {
        let (registered, folders) = if explicit.is_some() {
            (Vec::new(), Vec::new())
        } else {
            (system::registered_values(), system::program_folders())
        };
        let executable = candidate_paths(explicit, &registered, &folders)
            .into_iter()
            .find(|path| path.is_file());
        let version = executable.as_deref().and_then(system::version_of);
        classify(executable, version, floor)
    }
    #[cfg(not(windows))]
    {
        let _ = (explicit, floor);
        Probe::UnsupportedPlatform
    }
}

#[cfg(windows)]
mod system {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };

    use super::{
        BrowserVersion, PROGRAM_FOLDER_VALUES, PROGRAM_FOLDERS_LOCATION, REGISTERED_LOCATION,
        version_from_folder_names,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One string value, or `None` when it is absent, not a string or longer
    /// than any path.
    fn string_value(hive: HKEY, key: &str, value: Option<&str>) -> Option<String> {
        let key = wide(key);
        let value = value.map(wide);
        let value_name = value.as_ref().map_or(null(), |name| name.as_ptr());
        let mut buffer = [0u16; 1024];
        let mut bytes = size_of_val(&buffer) as u32;
        let status = unsafe {
            RegGetValueW(
                hive,
                key.as_ptr(),
                value_name,
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast::<c_void>(),
                &mut bytes,
            )
        };
        if status != 0 {
            return None;
        }
        let units = (bytes as usize / 2).min(buffer.len());
        let text = &buffer[..units];
        let end = text.iter().position(|unit| *unit == 0).unwrap_or(units);
        String::from_utf16(&text[..end]).ok()
    }

    /// The registered location's default value, for the person and then for
    /// the computer.
    pub(super) fn registered_values() -> Vec<String> {
        [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
            .into_iter()
            .filter_map(|hive| string_value(hive, REGISTERED_LOCATION, None))
            .collect()
    }

    pub(super) fn program_folders() -> Vec<String> {
        PROGRAM_FOLDER_VALUES
            .into_iter()
            .filter_map(|value| {
                string_value(HKEY_LOCAL_MACHINE, PROGRAM_FOLDERS_LOCATION, Some(value))
            })
            .collect()
    }

    /// The product version in the executable's version resource, or the
    /// install folder's layout when the resource cannot be read.
    pub(super) fn version_of(executable: &Path) -> Option<BrowserVersion> {
        resource_version(executable).or_else(|| layout_version(executable))
    }

    fn resource_version(executable: &Path) -> Option<BrowserVersion> {
        let path: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut ignored = 0u32;
        let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut ignored) };
        if size == 0 || size > 16 * 1024 * 1024 {
            return None;
        }
        // Words, so the fixed block inside is aligned for the read below.
        let mut block = vec![0u32; (size as usize).div_ceil(4)];
        if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
            return None;
        }
        let root = wide("\\");
        let mut found: *mut c_void = null_mut();
        let mut length = 0u32;
        let present = unsafe {
            VerQueryValueW(
                block.as_ptr().cast(),
                root.as_ptr(),
                &mut found,
                &mut length,
            )
        };
        if present == 0 || found.is_null() || (length as usize) < size_of::<VS_FIXEDFILEINFO>() {
            return None;
        }
        let info = unsafe { found.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
        if info.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some(BrowserVersion::from_fixed_info(
            info.dwProductVersionMS,
            info.dwProductVersionLS,
        ))
    }

    fn layout_version(executable: &Path) -> Option<BrowserVersion> {
        let names: Vec<String> = std::fs::read_dir(executable.parent()?)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        version_from_folder_names(names.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(explicit: Option<&str>, registered: &[&str], folders: &[&str]) -> Vec<String> {
        let registered: Vec<String> = registered.iter().map(|v| (*v).to_owned()).collect();
        let folders: Vec<String> = folders.iter().map(|v| (*v).to_owned()).collect();
        candidate_paths(explicit.map(Path::new), &registered, &folders)
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn m7_installed_probe_version_parsing() {
        assert_eq!(
            BrowserVersion::parse("131.0.2903.86"),
            Some(BrowserVersion::new(131, 0, 2903, 86))
        );
        assert_eq!(
            BrowserVersion::parse("0.0.0.0"),
            Some(BrowserVersion::new(0, 0, 0, 0))
        );
        assert_eq!(
            BrowserVersion::parse("65535.65535.65535.65535"),
            Some(BrowserVersion::new(65535, 65535, 65535, 65535))
        );
        for text in [
            "",
            "131",
            "131.0.2903",
            "131.0.2903.86.1",
            "131.0.2903.",
            ".0.2903.86",
            "131.0.2903.x",
            "131.0.2903.-1",
            "131.0.2903.+1",
            "131.0.2903. 86",
            "131.0.65536.86",
            "131.0.000001.86",
            "v131.0.2903.86",
            "SetupMetrics",
        ] {
            assert_eq!(BrowserVersion::parse(text), None, "{text:?}");
        }
        assert_eq!(
            BrowserVersion::new(131, 0, 2903, 86).to_string(),
            "131.0.2903.86"
        );
        assert_eq!(
            BrowserVersion::from_fixed_info(131 << 16, (2903 << 16) | 86),
            BrowserVersion::new(131, 0, 2903, 86)
        );
        // Ordered part by part, not as text.
        assert!(BrowserVersion::new(131, 0, 2903, 86) > BrowserVersion::new(99, 9, 9999, 999));
        assert!(BrowserVersion::new(131, 0, 2903, 86) < BrowserVersion::new(131, 0, 2903, 100));
    }

    #[test]
    fn m7_installed_probe_paths_from_fixtures() {
        // A caller's path is the only candidate, whatever is registered.
        assert_eq!(
            paths(
                Some(r"D:\scratch\browser.exe"),
                &[r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"],
                &[r"C:\Program Files"],
            ),
            [r"D:\scratch\browser.exe"]
        );
        // The registered location first, then each program folder, once.
        assert_eq!(
            paths(
                None,
                &[
                    r#""C:\Users\person\AppData\Local\Microsoft\Edge\Application\msedge.exe""#,
                    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                ],
                &[r"C:\Program Files (x86)\", r"C:\Program Files"],
            ),
            [
                r"C:\Users\person\AppData\Local\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            ]
        );
        // The same path spelled in another case is one candidate.
        assert_eq!(
            paths(
                None,
                &[r"c:\program files\microsoft\edge\application\MSEDGE.EXE"],
                &[r"C:\Program Files"],
            )
            .len(),
            1
        );
        // Nothing registered and no program folder: nothing to look at.
        assert!(paths(None, &[], &[]).is_empty());
    }

    #[test]
    fn m7_installed_probe_a_registered_value_must_be_the_executable_by_name() {
        assert_eq!(
            registered_path(r#"  "C:\Apps\msedge.exe"  "#),
            Some(PathBuf::from(r"C:\Apps\msedge.exe"))
        );
        assert_eq!(
            registered_path(r"\\server\share\MsEdge.EXE"),
            Some(PathBuf::from(r"\\server\share\MsEdge.EXE"))
        );
        for value in [
            "",
            "msedge.exe",
            r"Apps\msedge.exe",
            r"C:\Apps\other.exe",
            r"C:\Apps\msedge.exe --flag",
            r"C:\Apps\msedge.exe\",
            r"C:\Apps\notmsedge.exe.txt",
            r"C:msedge.exe",
        ] {
            assert_eq!(registered_path(value), None, "{value:?}");
        }
        // A program folder that is not a full path adds nothing.
        assert!(paths(None, &[], &["Program Files", ""]).is_empty());
    }

    #[test]
    fn m7_installed_probe_the_layout_names_the_highest_version() {
        assert_eq!(
            version_from_folder_names([
                "130.0.2849.80",
                "SetupMetrics",
                "131.0.2903.86",
                "9.9.9999.999",
                "131.0.2903",
            ]),
            Some(BrowserVersion::new(131, 0, 2903, 86))
        );
        assert_eq!(version_from_folder_names(["SetupMetrics", "Temp"]), None);
        assert_eq!(version_from_folder_names([]), None);
    }

    #[test]
    fn m7_installed_probe_below_the_floor_is_unsupported() {
        let executable = || Some(PathBuf::from(r"C:\Apps\msedge.exe"));
        let floor = Some(BrowserVersion::new(131, 0, 2903, 86));
        let found = |version| {
            Probe::Found(InstalledBrowser {
                executable: PathBuf::from(r"C:\Apps\msedge.exe"),
                version,
            })
        };
        let below = BrowserVersion::new(131, 0, 2903, 85);
        let at = BrowserVersion::new(131, 0, 2903, 86);
        let above = BrowserVersion::new(132, 0, 0, 0);
        assert_eq!(
            classify(executable(), Some(below), floor),
            Probe::UnsupportedVersion(below)
        );
        assert_eq!(classify(executable(), Some(at), floor), found(at));
        assert_eq!(classify(executable(), Some(above), floor), found(above));
        // With no floor set, no version is refused for being old.
        assert_eq!(classify(executable(), Some(below), None), found(below));
        assert_eq!(
            classify(executable(), None, floor),
            Probe::VersionUnreadable
        );
        assert_eq!(classify(None, Some(at), floor), Probe::NotInstalled);
        // The floor waits for the qualification.
        assert_eq!(VERSION_FLOOR, None);
    }

    #[test]
    fn m7_installed_probe_starts_no_process_and_shows_no_path() {
        // Everything above the tests: no way to start a process, and no
        // environment read.
        let source = include_str!("installed.rs");
        let code = source
            .split_once("#[cfg(test)]")
            .map_or(source, |(code, _)| code);
        for forbidden in [
            "process::Command",
            "Command::new",
            "CreateProcess",
            "ShellExecute",
            "env::var",
            "env!(",
        ] {
            assert!(!code.contains(forbidden), "{forbidden}");
        }
        let browser = InstalledBrowser {
            executable: PathBuf::from(r"C:\Users\person\AppData\Local\app.exe"),
            version: BrowserVersion::new(131, 0, 2903, 86),
        };
        let shown = format!("{:?}", Probe::Found(browser));
        assert!(!shown.contains("person"), "{shown}");
        assert!(shown.contains("131.0.2903.86"), "{shown}");
    }
}
