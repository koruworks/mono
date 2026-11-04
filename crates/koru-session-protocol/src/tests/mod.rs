// SPDX-License-Identifier: MPL-2.0

mod attachment;
mod delegation;
mod dependency;
mod ids;
mod negotiation;
mod project;
mod questions;
mod render;
mod schema;
mod state;
mod wire;

use std::str::FromStr;

use crate::*;

fn id<T: FromStr>(value: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    value.parse().unwrap()
}

fn session_id() -> SessionId {
    id("11111111-1111-4111-8111-111111111111")
}
fn attachment_id() -> AttachmentId {
    id("22222222-2222-4222-8222-222222222222")
}
fn turn_id() -> AgentTurnId {
    id("33333333-3333-4333-8333-333333333333")
}
fn idempotency_key() -> IdempotencyKey {
    id("44444444-4444-4444-8444-444444444444")
}
fn lease() -> ControllerLeaseToken {
    id("55555555-5555-4555-8555-555555555555")
}

fn seq(value: u64) -> StreamSequence {
    StreamSequence::new(value).unwrap()
}
fn terminal_generation(value: u64) -> TerminalGeneration {
    TerminalGeneration::new(value).unwrap()
}
fn viewport_generation(value: u64) -> ViewportGeneration {
    ViewportGeneration::new(value).unwrap()
}
