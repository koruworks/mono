// SPDX-License-Identifier: MPL-2.0

//! The three cohorts of piece F: `m8_wire_vectors`, `m8_wire_negotiation`
//! and `m8_wire_limits`, and the fence on what this crate may link.

mod limits;
mod negotiation;
mod surface;
mod vectors;

use std::collections::BTreeSet;

use koru_ai_core::bots::DeviceId;
use serde::de::DeserializeOwned;

use crate::{ClientHello, Negotiated, Role, client_capabilities, scope};

/// One golden vector: the exact bytes of one frame body or one record.
macro_rules! vector {
    ($name:literal) => {
        include_str!(concat!("../../vectors/", $name)).trim_end_matches(['\r', '\n'])
    };
}
pub(crate) use vector;

/// The fixture id whose last two hex digits are `suffix`.
pub(crate) fn id<T: DeserializeOwned>(suffix: &str) -> T {
    serde_json::from_str(&format!("\"00000000-0000-4000-8000-0000000000{suffix}\"")).unwrap()
}

pub(crate) fn owner() -> DeviceId {
    id("04")
}

pub(crate) fn client() -> DeviceId {
    id("c1")
}

pub(crate) fn all_scopes() -> BTreeSet<String> {
    [scope::READ, scope::CONTROL, scope::CONFIGURE]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn hello(role: Role) -> ClientHello {
    ClientHello {
        min_version: 1,
        max_version: 1,
        capabilities: client_capabilities(),
        role,
        client_device: client(),
        client_build: "0.0.3".to_owned(),
    }
}

pub(crate) fn supported() -> Vec<&'static str> {
    crate::BUILT_CAPABILITIES
        .iter()
        .map(|capability| capability.name)
        .collect()
}

/// A stream of `role` between this build's client and this build's owner,
/// with every scope.
pub(crate) fn negotiated(role: Role) -> Negotiated {
    Negotiated::owner_side(&hello(role), &client(), &all_scopes(), &supported()).unwrap()
}
