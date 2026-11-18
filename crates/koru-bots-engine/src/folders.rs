// SPDX-License-Identifier: MPL-2.0
//! Folder effects run on a creation/provider thread, never the engine actor.
//! The port deliberately has no remove, rename or recursive-create operation.
use std::{
    io,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub trait Folders {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf>;
    fn occupied(&self, path: &Path) -> io::Result<bool>;
    fn create(&self, path: &Path) -> io::Result<()>;
    fn directory(&self, path: &Path) -> io::Result<bool>;
    fn empty(&self, path: &Path) -> io::Result<bool>;
}

pub struct Native;
impl Folders for Native {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn occupied(&self, path: &Path) -> io::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }
    fn directory(&self, path: &Path) -> io::Result<bool> {
        Ok(std::fs::metadata(path)?.is_dir())
    }
    fn empty(&self, path: &Path) -> io::Result<bool> {
        Ok(std::fs::read_dir(path)?.next().is_none())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Invalid,
    Occupied,
    Mismatch,
    Reserved,
    Unavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A new durable intent must not adopt an occupied working folder.
    Create,
    /// An unfinished creation may have made its folder before a lost reply.
    RecoverCreation,
    /// An existing Bot's working folder may contain its prior work.
    Run,
}

pub fn managed_path(root: &Path, bot: Uuid) -> PathBuf {
    root.join("bots").join(bot.to_string()).join("workspace")
}

/// Drop a Windows verbatim prefix only after proving the ordinary spelling
/// resolves to the same directory. A trailing dot/space must not change cwd.
fn binding(fs: &dyn Folders, path: &Path) -> Result<PathBuf, Failure> {
    let text = path.to_str().ok_or(Failure::Invalid)?;
    let shown = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    };
    if shown.as_os_str().len() > 4096 || !shown.is_absolute() {
        return Err(Failure::Invalid);
    }
    if shown != path && fs.canonical(&shown).map_err(|_| Failure::Unavailable)? != path {
        return Err(Failure::Mismatch);
    }
    Ok(shown)
}

fn level(fs: &dyn Folders, path: &Path) -> Result<bool, Failure> {
    let made = match fs.create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(_) => return Err(Failure::Unavailable),
    };
    if fs.canonical(path).map_err(|_| Failure::Unavailable)? != path
        || !fs.directory(path).map_err(|_| Failure::Unavailable)?
    {
        return Err(Failure::Mismatch);
    }
    Ok(made)
}

/// Check each parent before creating its child. A pre-existing junction in
/// `bots` or the Bot folder must not redirect the creation into another tree.
pub fn managed(
    fs: &dyn Folders,
    root: &Path,
    bot: Uuid,
    purpose: Purpose,
) -> Result<PathBuf, Failure> {
    if bot.is_nil() {
        return Err(Failure::Invalid);
    }
    let root = fs.canonical(root).map_err(|_| Failure::Unavailable)?;
    if !fs.directory(&root).map_err(|_| Failure::Unavailable)? {
        return Err(Failure::Invalid);
    }
    let target = managed_path(&root, bot);
    if purpose == Purpose::Create && fs.occupied(&target).map_err(|_| Failure::Unavailable)? {
        return Err(Failure::Occupied);
    }
    level(fs, &root.join("bots"))?;
    level(fs, target.parent().ok_or(Failure::Invalid)?)?;
    let made = level(fs, &target)?;
    if !made {
        if purpose == Purpose::Create {
            return Err(Failure::Occupied);
        }
        if purpose == Purpose::RecoverCreation
            && !fs.empty(&target).map_err(|_| Failure::Unavailable)?
        {
            return Err(Failure::Mismatch);
        }
    }
    binding(fs, &target)
}

/// Selected folders are only inspected. Reserved roots must resolve too;
/// missing authority is never interpreted as permission to use Koru's data.
pub fn selected(fs: &dyn Folders, path: &Path, reserved: &[PathBuf]) -> Result<PathBuf, Failure> {
    let text = path.to_str().ok_or(Failure::Invalid)?;
    if !path.is_absolute() || text.len() > 4096 || text.contains('\0') {
        return Err(Failure::Invalid);
    }
    let path = fs.canonical(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Failure::Invalid
        } else {
            Failure::Unavailable
        }
    })?;
    if !fs.directory(&path).map_err(|_| Failure::Unavailable)? {
        return Err(Failure::Invalid);
    }
    for root in reserved {
        let root = fs.canonical(root).map_err(|_| Failure::Unavailable)?;
        if path.starts_with(root) {
            return Err(Failure::Reserved);
        }
    }
    binding(fs, &path)
}

#[cfg(test)]
mod tests;
