// SPDX-License-Identifier: MPL-2.0

//! A CLI's version, read out of whatever its `--version` prints.
//!
//! No two of them agree on a format — `2.1.312 (Claude Code)`,
//! `codex-cli 0.163.0`, `grok 1.0.60 (01a8e1c591c0) [stable]`,
//! `GitHub Copilot CLI 1.0.83.` — so the rule is the smallest one that reads
//! all of them: the first whitespace-separated token that parses as a semantic
//! version once a leading `v` and trailing punctuation are dropped.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(semver::Version);

impl Version {
    /// The first token of `output` that is a version, or `None` when the
    /// program printed none — an error, a usage line, nothing at all.
    pub fn first_in(output: &str) -> Option<Self> {
        output
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '[' || c == ']')
            .filter(|token| !token.is_empty())
            .find_map(|token| Self::from_str(token).ok())
    }
}

impl FromStr for Version {
    type Err = semver::Error;

    fn from_str(token: &str) -> Result<Self, Self::Err> {
        let token = token
            .trim()
            .trim_start_matches('v')
            .trim_end_matches(['.', ',', ';', ':']);
        semver::Version::parse(token).map(Self)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
