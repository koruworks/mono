// SPDX-License-Identifier: MPL-2.0

//! Private, positive proof that an exact attempt owns ordinary Run tools.
use koru_delegate_core::{AttemptGeneration, RunId};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RunAuthorityCredential(String);

impl RunAuthorityCredential {
    pub fn new(value: String) -> Option<Self> {
        (value.len() >= 32 && value.len() <= 256 && !value.contains('\0')).then_some(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .bytes()
                .zip(other.0.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl TryFrom<String> for RunAuthorityCredential {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value).ok_or("invalid Run authority credential")
    }
}

impl From<RunAuthorityCredential> for String {
    fn from(value: RunAuthorityCredential) -> Self {
        value.0
    }
}

impl std::fmt::Debug for RunAuthorityCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunAuthorityCredential([REDACTED])")
    }
}

pub trait RunAuthorityPort: Send + Sync {
    fn resolve(&self, run: RunId, generation: AttemptGeneration) -> Result<(), crate::Failure>;
}

#[derive(Clone)]
pub struct LocalRunAuthority {
    transport: crate::LocalDelegate,
    authority: RunAuthorityCredential,
}

impl LocalRunAuthority {
    pub fn new(endpoint: koru_ipc::Endpoint, authority: RunAuthorityCredential) -> Self {
        Self {
            transport: crate::LocalDelegate::new(endpoint, "run-authority"),
            authority,
        }
    }
}

impl RunAuthorityPort for LocalRunAuthority {
    fn resolve(&self, run: RunId, generation: AttemptGeneration) -> Result<(), crate::Failure> {
        match self
            .transport
            .call(&crate::ipc::Request::ResolveRunAuthority {
                authority: self.authority.clone(),
                run,
                generation,
            })? {
            crate::ipc::Response::RunAuthorityConfirmed {} => Ok(()),
            crate::ipc::Response::Failure(failure) => Err(failure),
            _ => Err(crate::Failure::new(
                "delegate_unexpected_response",
                "The Run authority owner received an unexpected response.",
                false,
            )),
        }
    }
}

#[cfg(test)]
mod tests;
