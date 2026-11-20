// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_schedule::RecurrenceRule;
use uuid::Uuid;
mod control;

fn id(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}
