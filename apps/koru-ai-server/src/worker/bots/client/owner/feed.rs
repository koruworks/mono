// SPDX-License-Identifier: MPL-2.0

//! The feed role: the owner's committed changes, written to one linked
//! device's stream by cursor (contract 2.5, 4.6, 5.1).
//!
//! What a page holds, who is never sent and when a page may be built are
//! the feed's own ([`super::super::feed`]). This is the stream around it:
//!
//! - a `subscribe` is answered after a reconcile of the markers with the
//!   Store: pages from its cursor when the cursor names history this
//!   database still has, and otherwise one `gap` and the same scan from 0
//!   with `reset: true` on every page;
//! - **flow control**: at most two pages are unacknowledged; a client that
//!   leaves a page unacknowledged for 60 seconds is disconnected, and that
//!   timer runs only while a page is outstanding;
//! - once the client is up to date, a page is built when the feed's head
//!   moves, and a `keepalive` is written every 30 seconds of silence;
//! - **the Store is asked for every page**, on a blocking thread, about the
//!   home workspaces of that page's Bots. A record of a Bot whose workspace
//!   is kept on this computer is never built, whatever its marker says;
//! - while the device is subscribed the markers are reconciled with the
//!   Store every 60 seconds, which is the only way a change of where a
//!   workspace is kept reaches a desktop that is already up to date.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use koru_bot_client_wire::{Keepalive, Refused, WireMessage, limits, refusal, scope};
use tokio::time::Instant;

use super::super::feed::{FeedRefusal, HomeCopies, PageOutcome, Subscription};
use super::{Gone, Read, Served, blocking};

/// One open feed stream, counted for the writer: it publishes the feed's
/// head only while one is open.
struct Following(Arc<AtomicUsize>);

impl Following {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(counter))
    }
}

impl Drop for Following {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How a feed stream ends.
enum End {
    /// The client closed it, or it can no longer be written.
    Closed,
    /// With this refusal as its last frame.
    Refuse(Refused),
    /// Without a frame: the client broke a timeout.
    Disconnect,
}

impl From<Gone> for End {
    fn from(_: Gone) -> Self {
        Self::Refuse(Refused::new(refusal::BOTS_UNAVAILABLE))
    }
}

/// Where one subscription is.
#[derive(Default)]
struct Subscribed {
    /// The cursor the next page starts above; `None` before a `subscribe`.
    cursor: Option<u64>,
    /// Every page is sent with `reset: true` until the client is up to date.
    reset: bool,
    /// The last page sent brought the client up to date.
    caught_up: bool,
    /// A page is owed whatever it holds: the first one after a `subscribe`,
    /// which tells the client the counts even when nothing changed.
    owed: bool,
    /// Whether this stream subscribed before: a fresh `subscribe` after the
    /// first is the client asking for a reset.
    again: bool,
    /// The end cursor of each page sent and not acknowledged, with when it
    /// was sent.
    outstanding: VecDeque<(u64, Instant)>,
    /// The published head at which a page build found nothing above the
    /// cursor: no page is asked for again until the head moves past it.
    idle_head: u64,
}

impl Subscribed {
    /// Whether a page should be built now, with the feed's head as the
    /// writer last published it.
    fn wants_page(&self, head: u64) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        self.outstanding.len() < limits::FEED_WINDOW as usize
            && (self.owed || !self.caught_up || (head > cursor && head > self.idle_head))
    }
}

/// Serve one feed stream until it ends.
pub(super) async fn serve(mut served: Served) {
    let _following = Following::new(&served.shared().feed_streams);
    match follow(&mut served).await {
        End::Closed => {}
        End::Refuse(refusal) => served.stream.refuse(refusal).await,
        End::Disconnect => served.stream.disconnect(),
    }
}

async fn follow(served: &mut Served) -> End {
    let timing = served.shared().timing;
    let mut head = served.shared().head.clone();
    let mut feed = Subscribed::default();
    let mut written = Instant::now();
    let mut reconcile_at = Instant::now() + timing.reconcile;
    loop {
        loop {
            let published = *head.borrow_and_update();
            if !feed.wants_page(published) {
                break;
            }
            match page(served, &mut feed, published).await {
                Ok(true) => written = Instant::now(),
                Ok(false) => {}
                Err(end) => return end,
            }
        }
        let ack_at = feed.outstanding.front().map(|(_, sent)| *sent + timing.ack);
        tokio::select! {
            read = served.stream.read() => {
                let message = match read {
                    Read::Message(message) => message,
                    Read::Closed => return End::Closed,
                    Read::Malformed => {
                        return End::Refuse(Refused::new(refusal::PROTOCOL_ERROR));
                    }
                };
                if let Err(refusal) = served.check(&message, scope::READ) {
                    return End::Refuse(refusal);
                }
                match message {
                    WireMessage::Subscribe(subscribe) => {
                        if let Err(end) = self::subscribe(served, &mut feed, subscribe).await {
                            return end;
                        }
                        written = Instant::now();
                        reconcile_at = Instant::now() + timing.reconcile;
                    }
                    WireMessage::Ack(ack) => {
                        // An acknowledgement of something that was never
                        // sent is not this protocol.
                        if feed.cursor.is_none_or(|cursor| ack.cursor > cursor) {
                            return End::Refuse(Refused::new(refusal::PROTOCOL_ERROR));
                        }
                        while feed
                            .outstanding
                            .front()
                            .is_some_and(|(end, _)| *end <= ack.cursor)
                        {
                            feed.outstanding.pop_front();
                        }
                    }
                    // `check` let only this role's two messages through.
                    _ => return End::Refuse(Refused::new(refusal::PROTOCOL_ERROR)),
                }
            }
            changed = head.changed() => {
                // The worker went away with the writer's half.
                if changed.is_err() {
                    return End::Closed;
                }
            }
            () = tokio::time::sleep_until(ack_at.unwrap_or(written)), if ack_at.is_some() => {
                return End::Disconnect;
            }
            () = tokio::time::sleep_until(written + timing.keepalive) => {
                if served
                    .stream
                    .send(WireMessage::Keepalive(Keepalive {}))
                    .await
                    .is_err()
                {
                    return End::Closed;
                }
                written = Instant::now();
            }
            () = tokio::time::sleep_until(reconcile_at), if feed.cursor.is_some() => {
                if let Err(gone) = served.shared().reconcile(false).await {
                    return gone.into();
                }
                reconcile_at = Instant::now() + timing.reconcile;
            }
        }
    }
}

/// Answer one `subscribe`: reconcile, decide, and write the `gap` when the
/// cursor is not accepted. The pages follow from the loop.
async fn subscribe(
    served: &mut Served,
    feed: &mut Subscribed,
    subscribe: koru_bot_client_wire::Subscribe,
) -> Result<(), End> {
    served
        .names_this_owner(&subscribe.owner_device)
        .map_err(End::Refuse)?;
    served.shared().reconcile(true).await?;
    let again = feed.again;
    let decision = served
        .shared()
        .link
        .on_writer(move |state| {
            let feed = state.bot_clients.feed().ok_or(FeedRefusal::Unavailable)?;
            feed.subscribe(&state.repository, &subscribe, again)
        })
        .await?;
    let decision = decision.map_err(|refused| {
        End::Refuse(Refused::new(match refused {
            FeedRefusal::WrongOwner => refusal::WRONG_OWNER,
            FeedRefusal::Malformed => refusal::PROTOCOL_ERROR,
            FeedRefusal::Unavailable => refusal::BOTS_UNAVAILABLE,
        }))
    })?;
    *feed = Subscribed {
        again: true,
        owed: true,
        ..Subscribed::default()
    };
    match decision {
        Subscription::Continue { cursor } => feed.cursor = Some(cursor),
        Subscription::Reset(gap) => {
            let reason = gap.reason;
            served
                .stream
                .send(WireMessage::Gap(gap))
                .await
                .map_err(|_| End::Closed)?;
            // The client was told: its copy is rebuilt from here.
            served.shared().events.reset(served.stream.device, reason);
            feed.cursor = Some(0);
            feed.reset = true;
        }
    }
    Ok(())
}

/// Build the next page and write it. Answers whether a frame was written:
/// a page that holds nothing is written only when one is owed.
async fn page(served: &mut Served, feed: &mut Subscribed, published: u64) -> Result<bool, End> {
    let Some(cursor) = feed.cursor else {
        return Ok(false);
    };
    let shared = Arc::clone(served.shared());
    // Asked afresh for every page: what the Store said for an earlier page
    // is not what it says now.
    let mut copies = HomeCopies::default();
    // Each round asks about workspaces not yet asked about, or reconciles
    // once; neither can repeat without end.
    for _ in 0..6 {
        match shared.page(cursor, feed.reset, copies.clone()).await? {
            PageOutcome::Ask(homes) => {
                let authority = Arc::clone(&shared.authority);
                copies.merge(blocking(move || HomeCopies::ask(authority.as_ref(), homes)).await?);
            }
            PageOutcome::Reconcile => {
                shared.reconcile(true).await?;
                copies = HomeCopies::default();
            }
            PageOutcome::Page(changes) => {
                if changes.entries.is_empty() && !feed.owed {
                    // Nothing above the cursor after all.
                    feed.caught_up = true;
                    feed.idle_head = published;
                    return Ok(false);
                }
                let (to_cursor, caught_up) = (changes.to_cursor, changes.caught_up);
                served
                    .stream
                    .send(WireMessage::Changes(changes))
                    .await
                    .map_err(|_| End::Closed)?;
                feed.outstanding.push_back((to_cursor, Instant::now()));
                feed.cursor = Some(to_cursor);
                feed.caught_up = caught_up;
                feed.owed = false;
                if caught_up {
                    feed.reset = false;
                }
                return Ok(true);
            }
        }
    }
    Err(End::Refuse(Refused::new(refusal::BOTS_UNAVAILABLE)))
}
