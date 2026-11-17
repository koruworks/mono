// SPDX-License-Identifier: MPL-2.0

//! Following one owner's feed: the dial, the hello, the subscription, and
//! every page applied whole on the AI writer and then acknowledged.
//!
//! **Every page is checked three ways before a row is written** (contract
//! 4.6 and 5.2): against the stream (the feed and start of the hello, the
//! cursor this copy holds, ascending `seq` values, the limits the owner
//! announced), against itself (each record decodes as the format this build
//! reads, its ids are its entry's, a conversation's Bot and a message's
//! conversation are the entry's, a Bot names this owner), and against the
//! database (no id belongs to this computer or to another owner). A page
//! that fails any of them is not applied, the stream is closed, and the
//! owner is dialed again.
//!
//! **A reset is never guessed.** The owner says `gap`, then sends the whole
//! copy from 0 with `reset` on every page. Those pages are staged and
//! switched in by the one that brings the copy up to date. When a copy's
//! row counts differ from the counts the owner sends with such a page, this
//! asks for one reset, at most once per connection.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use koru_ai_core::bots::{DeviceId, LinkedOwnerState};
use koru_ai_sqlite::{MirrorCounts, MirrorRefusal};
use koru_bot_client_wire::{
    Ack, Changes, ClientHello, MAX_VERSION, MIN_VERSION, Negotiated, OwnerHello, PageExpectation,
    Role, Subscribe, Violation, WireMessage, client_capabilities, refusal,
};
use koru_network_edge::{NetworkTarget, ProtocolToken};

use super::{Owner, Read, Shared, Wire, copy, lock, now_ms};
use crate::worker::bots::events::{Copies, CopyChange};

/// Why a stream to an owner could not be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Open {
    /// The dial failed, the owner did not answer, or it refused for a
    /// reason that may pass. A peer whose build has no Bot protocol, and a
    /// phone, end here: a failed dial never means "needs an update".
    Unreachable,
    /// The owner answered that the two share no protocol version, or sent
    /// a copy format this build does not read.
    Unsupported,
}

/// How one connection to an owner ended.
enum End {
    Unreachable,
    Unsupported,
    /// The stream ended after its hello. `caught_up` says whether the copy
    /// was brought up to date on it.
    Lost {
        caught_up: bool,
    },
    /// The worker is stopping.
    Gone,
}

impl Shared {
    /// Open one stream to `owner` in `role` and read its answer to the
    /// hello.
    pub(super) async fn open(
        &self,
        owner: DeviceId,
        role: Role,
    ) -> Result<(Wire, OwnerHello, Negotiated), Open> {
        let this = self.this_device().await.ok_or(Open::Unreachable)?;
        let token = ProtocolToken::parse(koru_bot_client_wire::PROTOCOL_TOKEN)
            .map_err(|_| Open::Unreachable)?;
        let dial = self
            .egress
            .open(NetworkTarget::Device { device_id: owner }, token);
        let session = match tokio::time::timeout(self.timing.hello, dial).await {
            Ok(Ok(session)) => session,
            // Every failure of a dial is one answer: not reachable.
            Ok(Err(_)) | Err(_) => return Err(Open::Unreachable),
        };
        let mut wire = Wire::new(session);
        let hello = ClientHello {
            min_version: MIN_VERSION,
            max_version: MAX_VERSION,
            capabilities: client_capabilities(),
            role,
            client_device: this,
            client_build: env!("CARGO_PKG_VERSION").into(),
        };
        wire.send(WireMessage::ClientHello(hello.clone()), self.timing.hello)
            .await
            .map_err(|()| Open::Unreachable)?;
        match wire.read(self.timing.hello).await {
            Read::Message(WireMessage::OwnerHello(answer)) => {
                // An answer in another computer's name is not this owner's.
                if answer.owner_device != owner {
                    return Err(Open::Unreachable);
                }
                match Negotiated::client_side(&hello, &answer) {
                    Ok(negotiated) => Ok((wire, answer, negotiated)),
                    Err(Violation::UnknownReplicaFormat) => Err(Open::Unsupported),
                    Err(_) => Err(Open::Unreachable),
                }
            }
            Read::Message(WireMessage::Refused(refused))
                if refused.reason == refusal::UNSUPPORTED_VERSION =>
            {
                Err(Open::Unsupported)
            }
            Read::Message(WireMessage::Refused(refused)) => {
                // Only a reason this build knows is logged: the text came
                // from the other computer.
                let reason = refusal::ALL
                    .iter()
                    .find(|known| **known == refused.reason)
                    .copied()
                    .unwrap_or("other");
                tracing::debug!(reason, "a Bot owner refused a stream");
                Err(Open::Unreachable)
            }
            _ => Err(Open::Unreachable),
        }
    }

    /// Keep a feed stream to `owner` open for as long as the mirror runs:
    /// connect, follow, and when the stream ends say so in the copy's
    /// state and dial again after a pause that grows while the owner stays
    /// away.
    pub(super) async fn follow(self: Arc<Self>, owner: Arc<Owner>) {
        let device = owner.device;
        let mut backoff = self.timing.backoff_first;
        loop {
            if self.link.stopping() {
                return;
            }
            let end = self.connection(&owner).await;
            owner.connected.store(false, Ordering::Release);
            *lock(&owner.negotiated) = None;
            let unsupported = matches!(end, End::Unsupported);
            let now = now_ms();
            // No subscription is open: a copy that was current is stale. A
            // copy nothing was ever received for stays `never`.
            let marked = self
                .link
                .stored(move |state| {
                    let Some(row) = state.repository.mirror_owner(device)? else {
                        return Ok(());
                    };
                    let (to, why) = if unsupported {
                        (LinkedOwnerState::Unsupported, CopyChange::NeedsUpdate)
                    } else if row.state == LinkedOwnerState::Current {
                        (LinkedOwnerState::Stale, CopyChange::StreamLost)
                    } else {
                        return Ok(());
                    };
                    state.mirror_change(Copies::One(device), why, false, |state| {
                        state.repository.mirror_set_state(device, to, now)
                    })
                })
                .await;
            if marked.is_none() || matches!(end, End::Gone) {
                return;
            }
            let pause = match end {
                End::Unsupported => self.timing.unsupported_retry,
                End::Lost { caught_up: true } => {
                    backoff = self.timing.backoff_first;
                    backoff
                }
                _ => {
                    let pause = backoff;
                    backoff = (backoff * 2).min(self.timing.backoff_max);
                    pause
                }
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// One connection: hello, subscribe, pages until the stream ends.
    async fn connection(self: &Arc<Self>, owner: &Arc<Owner>) -> End {
        let device = owner.device;
        let (mut wire, hello, negotiated) = match self.open(device, Role::Feed).await {
            Ok(opened) => opened,
            Err(Open::Unsupported) => return End::Unsupported,
            Err(Open::Unreachable) => return End::Unreachable,
        };
        let version = negotiated.version;
        *lock(&owner.negotiated) = Some(negotiated.clone());
        owner.connected.store(true, Ordering::Release);
        // A reset that an earlier connection left half way is dropped
        // before this one subscribes.
        let now = now_ms();
        let Ok(row) = self
            .link
            .on_writer(move |state| {
                // What an interrupted reset left is dropped here; a copy
                // with nothing staged is not touched.
                state.mirror_change(
                    Copies::One(device),
                    CopyChange::ResetDropped,
                    false,
                    |state| state.repository.mirror_discard_staging(device, now),
                )?;
                state.repository.mirror_owner(device)
            })
            .await
        else {
            return End::Gone;
        };
        let Ok(Some(row)) = row else {
            return End::Lost { caught_up: false };
        };
        let subscribe = match (&row.feed_id, &row.boot_id) {
            (Some(feed_id), Some(boot_id)) => Subscribe {
                owner_device: device,
                feed_id: Some(feed_id.clone()),
                boot_id: Some(boot_id.clone()),
                cursor: Some(row.cursor),
            },
            _ => Subscribe::fresh(device),
        };
        if wire
            .send(WireMessage::Subscribe(subscribe), self.timing.hello)
            .await
            .is_err()
        {
            return End::Lost { caught_up: false };
        }
        tracing::debug!(cursor = row.cursor, "following a Bot owner's feed");
        // The cursor the next page must start from, whether pages are being
        // staged for a reset, and the row counts of the visible copy and of
        // the staged one.
        let mut expected = row.cursor;
        let mut resetting = false;
        let mut asked_reset = false;
        let mut caught_up = false;
        let mut held: Option<MirrorCounts> = None;
        let mut staged = MirrorCounts::default();
        loop {
            let message = match wire.read(self.timing.silence).await {
                Read::Message(message) => message,
                Read::Closed | Read::Silent | Read::Malformed => {
                    return End::Lost { caught_up };
                }
            };
            if negotiated.check_from_owner(&message).is_err() {
                return End::Lost { caught_up };
            }
            let page: Changes = match message {
                WireMessage::Keepalive(_) => continue,
                WireMessage::Gap(gap) => {
                    if gap.owner_device != device {
                        return End::Lost { caught_up };
                    }
                    resetting = true;
                    expected = 0;
                    staged = MirrorCounts::default();
                    continue;
                }
                WireMessage::Changes(page) => page,
                // A refusal, or anything a feed does not carry.
                _ => return End::Lost { caught_up },
            };
            let expectation = PageExpectation {
                owner_device: &device,
                feed_id: &hello.feed_id,
                boot_id: &hello.boot_id,
                cursor: expected,
            };
            if page.reset != resetting
                || page.check_page(&expectation).is_err()
                || page.check_limits(&hello.limits).is_err()
            {
                tracing::warn!(
                    from = page.from_cursor,
                    to = page.to_cursor,
                    "a Bot owner's page was not the next one; the stream is closed"
                );
                return End::Lost { caught_up };
            }
            let to_cursor = page.to_cursor;
            let live = page.live;
            let Ok((mirror_page, learned)) = copy::page(device, version, &page) else {
                tracing::warn!(
                    to = to_cursor,
                    "a Bot owner's page held a record that is not its entry's; the stream is closed"
                );
                return End::Lost { caught_up };
            };
            drop(page);
            let now = now_ms();
            // A page that begins a reset from the start: the copy's row
            // looks the same before and after it when one was already being
            // received.
            let began_again = mirror_page.reset && mirror_page.from_cursor == 0;
            let applied = match self
                .link
                .on_writer(move |state| {
                    // A page that is refused writes nothing and says
                    // nothing; so does one that was sent again.
                    state.mirror_change(
                        Copies::One(device),
                        CopyChange::CaughtUp,
                        began_again,
                        |state| state.repository.mirror_apply_page(&mirror_page, now),
                    )
                })
                .await
            {
                Err(_) => return End::Gone,
                Ok(Err(refusal)) => {
                    tracing::warn!(
                        to = to_cursor,
                        claimed = refusal == MirrorRefusal::Claimed,
                        "a Bot owner's page was refused; the stream is closed"
                    );
                    return End::Lost { caught_up };
                }
                Ok(Ok(applied)) => applied,
            };
            if resetting {
                add(&mut staged, applied.delta);
            } else if let Some(held) = held.as_mut() {
                add(held, applied.delta);
            }
            if applied.switched {
                held = Some(staged);
                resetting = false;
            }
            if wire
                .send(
                    WireMessage::Ack(Ack { cursor: to_cursor }),
                    self.timing.hello,
                )
                .await
                .is_err()
            {
                return End::Lost { caught_up };
            }
            expected = to_cursor;
            lock(&self.index).learn(device, learned);
            if !resetting {
                owner.cursor.send_replace(to_cursor);
            }
            let Some(live) = live else {
                continue;
            };
            // The page brought the copy up to date. The owner's counts are
            // exact; a copy that holds other counts asks for one reset.
            let counts = match held {
                Some(counts) => counts,
                None => {
                    let Some(counts) = self
                        .link
                        .stored(move |state| state.repository.mirror_counts(device))
                        .await
                    else {
                        return End::Gone;
                    };
                    held = Some(counts);
                    counts
                }
            };
            let equal = counts.bots == i64::from(live.bots)
                && counts.chats == i64::from(live.chats)
                && counts.messages == i64::from(live.messages);
            if !equal && !asked_reset {
                asked_reset = true;
                tracing::warn!(
                    "a copy of a Bot owner's feed holds other counts; asking for a reset"
                );
                if wire
                    .send(
                        WireMessage::Subscribe(Subscribe::fresh(device)),
                        self.timing.hello,
                    )
                    .await
                    .is_err()
                {
                    return End::Lost { caught_up };
                }
                continue;
            }
            if !caught_up {
                caught_up = true;
                // What was sent and never answered is asked about again,
                // once per connection.
                self.runtime
                    .spawn(Arc::clone(self).resend(Arc::clone(owner)));
            }
        }
    }
}

fn add(counts: &mut MirrorCounts, delta: MirrorCounts) {
    counts.bots += delta.bots;
    counts.chats += delta.chats;
    counts.messages += delta.messages;
}
