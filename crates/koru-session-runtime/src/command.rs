// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use koru_session_protocol::GridSize;

use crate::{TransportError, TransportResult, TransportStage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentPolicy {
    Inherit,
    Clean,
    InheritWith {
        set: BTreeMap<OsString, OsString>,
        remove: BTreeSet<OsString>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    pub argv: Vec<OsString>,
    /// Already-resolved working directory. The runtime never discovers a root.
    pub cwd: PathBuf,
    pub environment: EnvironmentPolicy,
    pub size: GridSize,
}

impl SpawnRequest {
    pub fn new(
        argv: impl IntoIterator<Item = impl Into<OsString>>,
        cwd: PathBuf,
        size: GridSize,
    ) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd,
            environment: EnvironmentPolicy::Inherit,
            size,
        }
    }

    pub fn platform_shell(cwd: PathBuf, size: GridSize) -> Self {
        Self::new([default_shell()], cwd, size)
    }

    pub fn validate(&self) -> TransportResult<()> {
        if self.argv.is_empty()
            || self.argv[0].is_empty()
            || !self.cwd.is_absolute()
            || !self.cwd.is_dir()
            || contains_nul(self.cwd.as_os_str())
            || self.size.validate().is_err()
            || self.argv.iter().any(|arg| contains_nul(arg))
            || !valid_environment_policy(&self.environment)
        {
            return Err(TransportError::stage(TransportStage::InvalidRequest));
        }
        Ok(())
    }

    /// The child's whole environment. An inheriting one carries the
    /// **effective `PATH`** (`koru_session_profiles::effective_path`) in
    /// place of this process's, before the request's own `set` and `remove`
    /// apply — so a profile that names `PATH` still decides it. A terminal
    /// tab, a raw agent tab and an *Install* tab of a Koru opened from Finder
    /// would otherwise run with launchd's four directories, where neither
    /// `node` nor `npm` nor any agent is. `Clean` stays clean.
    pub(crate) fn environment(&self) -> Vec<(OsString, OsString)> {
        self.environment_with_path(koru_session_profiles::effective_path)
    }

    /// [`environment`](Self::environment) with the effective `PATH` supplied,
    /// so the composition is tested without computing it.
    pub(crate) fn environment_with_path(
        &self,
        effective_path: impl FnOnce() -> OsString,
    ) -> Vec<(OsString, OsString)> {
        let mut environment: Vec<_> = match &self.environment {
            EnvironmentPolicy::Clean => Vec::new(),
            EnvironmentPolicy::Inherit | EnvironmentPolicy::InheritWith { .. } => {
                std::env::vars_os().collect()
            }
        };
        if !matches!(self.environment, EnvironmentPolicy::Clean) {
            let path = effective_path();
            if !path.is_empty() {
                let key = OsString::from("PATH");
                environment.retain(|(current, _)| !same_environment_key(current, &key));
                environment.push((key, path));
            }
        }
        if let EnvironmentPolicy::InheritWith { set, remove } = &self.environment {
            for key in remove {
                environment.retain(|(current, _)| !same_environment_key(current, key));
            }
            for (key, value) in set {
                environment.retain(|(current, _)| !same_environment_key(current, key));
                environment.push((key.clone(), value.clone()));
            }
        }
        environment
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
}

fn valid_environment_policy(policy: &EnvironmentPolicy) -> bool {
    let EnvironmentPolicy::InheritWith { set, remove } = policy else {
        return true;
    };
    set.iter()
        .all(|(key, value)| valid_environment_key(key) && !contains_nul(value))
        && remove.iter().all(|key| valid_environment_key(key))
}

fn valid_environment_key(key: &OsStr) -> bool {
    !key.is_empty() && !contains_nul(key) && !key.to_string_lossy().contains('=')
}

#[cfg(windows)]
fn same_environment_key(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    fn folded(value: &OsStr) -> Vec<u16> {
        value
            .encode_wide()
            .map(|unit| match unit {
                0x41..=0x5a => unit + 0x20,
                _ => unit,
            })
            .collect()
    }

    folded(left) == folded(right)
}

#[cfg(not(windows))]
fn same_environment_key(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

fn contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

pub fn default_shell() -> OsString {
    #[cfg(windows)]
    {
        std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe"))
    }
    #[cfg(unix)]
    {
        std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"))
    }
}

#[cfg(test)]
mod tests;
