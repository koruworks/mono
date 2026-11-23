// SPDX-License-Identifier: MPL-2.0

//! Device-local ordered references. This collection is deliberately outside
//! the scalar registry and session files. Call only from an I/O worker.
use crate::{SettingsPaths, storage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{self, Read},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_SCOPES: usize = 128;
pub const MAX_PINS: usize = 128;
pub const LOCK_WAIT: Duration = Duration::from_secs(2);
static WRITES: Mutex<()> = Mutex::new(());

/// Stable opaque UUID, represented without bringing another authority into
/// this leaf crate. The gateway also validates domain identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
#[schemars(with = "String", rename = "MiniAppPinId")]
pub struct PinId(pub u128);
impl Serialize for PinId {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:032x}", self.0))
    }
}
impl<'de> Deserialize<'de> for PinId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let id = u128::from_str_radix(&value, 16).map_err(serde::de::Error::custom)?;
        if value != format!("{id:032x}") || id == 0 {
            return Err(serde::de::Error::custom("invalid pin identity"));
        }
        Ok(Self(id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "MiniAppPinsScope")]
pub struct Scope {
    pub profile: String,
    pub workspace: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "MiniAppPin")]
pub struct Pin {
    pub id: PinId,
    pub family: String,
    pub version: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "placement", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "MiniAppPinPlacement")]
pub enum Placement {
    First,
    Last,
    Before { neighbor: PinId },
    After { neighbor: PinId },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "MiniAppPinMutation")]
pub enum Mutation {
    Add { pin: Pin },
    Remove { id: PinId },
    Move { id: PinId, to: Placement },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "MiniAppPinsRequest")]
pub struct Request {
    pub scope: Scope,
    pub mutation: Mutation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    scope: Scope,
    pins: Vec<Pin>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    version: u32,
    scopes: Vec<Entry>,
}
impl Default for File {
    fn default() -> Self {
        Self {
            version: 1,
            scopes: vec![],
        }
    }
}

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self(format!("pin storage failed: {e}"))
    }
}
pub type Result<T> = std::result::Result<T, Error>;
fn invalid() -> Error {
    Error("The mini-app pin file or operation is unsupported or invalid; it was preserved.".into())
}
fn identifier(s: &str) -> bool {
    s.len() == 36
        && s != "00000000-0000-0000-0000-000000000000"
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        if identifier(&self.profile) && identifier(&self.workspace) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
impl Pin {
    pub fn validate(&self) -> Result<()> {
        if self.id.0 != 0
            && identifier(&self.family)
            && identifier(&self.version)
            && !self.title.is_empty()
            && self.title.chars().count() <= 256
            && !self.title.chars().any(char::is_control)
        {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
impl File {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid());
        }
        let file: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if file.version != 1 || file.scopes.len() > MAX_SCOPES {
            return Err(invalid());
        }
        let mut scopes = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for entry in &file.scopes {
            entry.scope.validate()?;
            if !scopes.insert((&entry.scope.profile, &entry.scope.workspace))
                || entry.pins.len() > MAX_PINS
            {
                return Err(invalid());
            }
            let mut versions = BTreeSet::new();
            for pin in &entry.pins {
                pin.validate()?;
                if !identities.insert(pin.id) || !versions.insert(&pin.version) {
                    return Err(invalid());
                }
            }
        }
        Ok(file)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        Self::decode(&bytes)?;
        Ok(bytes)
    }
    pub fn list(&self, scope: &Scope) -> Vec<Pin> {
        self.scopes
            .iter()
            .find(|e| e.scope == *scope)
            .map_or_else(Vec::new, |e| e.pins.clone())
    }
    pub fn apply(&mut self, scope: &Scope, mutation: &Mutation) -> Result<Vec<Pin>> {
        scope.validate()?;
        let mut pins = self.list(scope);
        match mutation {
            Mutation::Add { pin } => {
                pin.validate()?;
                if let Some(existing) = pins.iter().find(|p| p.version == pin.version) {
                    if existing.family != pin.family {
                        return Err(invalid());
                    }
                } else {
                    if pins.len() >= MAX_PINS
                        || self
                            .scopes
                            .iter()
                            .flat_map(|e| &e.pins)
                            .any(|p| p.id == pin.id)
                    {
                        return Err(invalid());
                    }
                    pins.push(pin.clone());
                }
            }
            Mutation::Remove { id } => pins.retain(|p| p.id != *id),
            Mutation::Move { id, to } => {
                let from = pins.iter().position(|p| p.id == *id).ok_or_else(invalid)?;
                if matches!(to, Placement::Before { neighbor } | Placement::After { neighbor } if neighbor == id)
                {
                    return Err(invalid());
                }
                let pin = pins.remove(from);
                let index = match to {
                    Placement::First => 0,
                    Placement::Last => pins.len(),
                    Placement::Before { neighbor } => pins
                        .iter()
                        .position(|p| p.id == *neighbor)
                        .ok_or_else(invalid)?,
                    Placement::After { neighbor } => {
                        pins.iter()
                            .position(|p| p.id == *neighbor)
                            .ok_or_else(invalid)?
                            + 1
                    }
                };
                pins.insert(index, pin);
            }
        }
        if pins.is_empty() {
            self.scopes.retain(|e| e.scope != *scope);
        } else if let Some(entry) = self.scopes.iter_mut().find(|e| e.scope == *scope) {
            entry.pins = pins.clone();
        } else if !pins.is_empty() {
            if self.scopes.len() >= MAX_SCOPES {
                return Err(invalid());
            }
            self.scopes.push(Entry {
                scope: scope.clone(),
                pins: pins.clone(),
            });
        }
        self.encode()?;
        Ok(pins)
    }
}

/// Storage/serialization port keeps unit fixtures entirely in memory.
pub trait Storage {
    fn transact<T>(
        &self,
        action: impl FnOnce(Option<Vec<u8>>) -> Result<(T, Option<Vec<u8>>)>,
    ) -> Result<T>;
}
pub fn list(storage: &impl Storage, scope: &Scope) -> Result<Vec<Pin>> {
    scope.validate()?;
    storage.transact(|bytes| Ok((read(bytes)?.list(scope), None)))
}
pub fn mutate(storage: &impl Storage, request: &Request) -> Result<Vec<Pin>> {
    request.scope.validate()?;
    storage.transact(|bytes| {
        let mut file = read(bytes)?;
        let pins = file.apply(&request.scope, &request.mutation)?;
        Ok((pins, Some(file.encode()?)))
    })
}
fn read(bytes: Option<Vec<u8>>) -> Result<File> {
    bytes.map_or_else(|| Ok(File::default()), |b| File::decode(&b))
}

#[derive(Clone)]
pub struct Authority {
    paths: SettingsPaths,
}
impl Authority {
    pub fn new(paths: SettingsPaths) -> Self {
        Self { paths }
    }
    pub fn list(&self, scope: &Scope) -> Result<Vec<Pin>> {
        list(self, scope)
    }
    pub fn mutate(&self, request: &Request) -> Result<Vec<Pin>> {
        mutate(self, request)
    }
}
impl Storage for Authority {
    fn transact<T>(
        &self,
        action: impl FnOnce(Option<Vec<u8>>) -> Result<(T, Option<Vec<u8>>)>,
    ) -> Result<T> {
        let deadline = Instant::now() + LOCK_WAIT;
        let _guard = loop {
            match WRITES.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(Error("Pin storage lock failed.".into()));
                }
                Err(_) if Instant::now() >= deadline => {
                    return Err(Error("Pin storage is busy. Try again.".into()));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let destination = self.paths.mini_app_pins_file();
        let parent = destination.parent().ok_or_else(invalid)?;
        std::fs::create_dir_all(parent)?;
        let sidecar = destination.with_extension("json.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(sidecar)?;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(Error("Pin storage is busy. Try again.".into()));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        let bytes = match std::fs::File::open(&destination) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
                if bytes.len() > MAX_BYTES {
                    return Err(invalid());
                }
                Some(bytes)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let (value, write) = action(bytes)?;
        if let Some(bytes) = write {
            storage::write_atomic(&destination, &bytes)?;
        }
        // Dropping the fresh handle unlocks; the stable sidecar is never removed.
        Ok(value)
    }
}

#[cfg(test)]
mod tests;
