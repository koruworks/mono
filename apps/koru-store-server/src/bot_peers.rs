// SPDX-License-Identifier: MPL-2.0

//! The admission port of the Bot client protocol (M8): which linked devices
//! are something to this computer's Bots.
//!
//! The Store worker already receives the Agent's whole authorization
//! snapshot on the sync control channel. This port is one read over it. It
//! asks the Agent nothing and opens nothing.
//!
//! **Its answer is neutral.** It says what a peer is for Bots, a client of
//! this computer's Bots and an owner whose Bots this computer may keep a
//! copy of, and with which scopes. It does not say which rule made it so. In
//! M8 the only rule is the own-device rule: a device paired directly with
//! all data is both, with every scope. A later source of peers changes what
//! fills this port and none of its callers.
//!
//! **Not told is not the same as told nothing.** Before the first snapshot
//! of this process the answer has sequence 0 and names nobody. A caller that
//! acts on a peer's absence, such as deleting a copy, must wait for a
//! sequence above 0. The answer is kept when the Agent's control session
//! ends, for the same reason: an Agent that restarts has not unlinked
//! anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use koru_device::DeviceId;
use koru_sync_control::AuthorizationSnapshot;

/// The scope of reading a computer's Bots.
pub const SCOPE_READ: &str = "read";
/// The scope of controlling them: chat, Stop, steer, Pause, Resume, answers.
pub const SCOPE_CONTROL: &str = "control";
/// The scope of configuring them: name, model, mode.
pub const SCOPE_CONFIGURE: &str = "configure";

/// What one peer device is to this computer's Bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotPeer {
    /// This peer may connect to this computer's Bots.
    pub client: bool,
    /// This computer may keep a copy of that peer's Bots.
    pub owner: bool,
    /// What it may do here as a client.
    pub scopes: BTreeSet<String>,
}

/// The port's answer at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotPeersSnapshot {
    /// Moves by one each time the answer changes. 0 until this process was
    /// told anything.
    pub sequence: u64,
    pub peers: BTreeMap<DeviceId, BotPeer>,
}

impl BotPeersSnapshot {
    /// The scopes `device` holds as a client of this computer's Bots, or
    /// `None` when the port does not name it as a client.
    pub fn client_scopes(&self, device: &DeviceId) -> Option<&BTreeSet<String>> {
        self.peers
            .get(device)
            .filter(|peer| peer.client)
            .map(|peer| &peer.scopes)
    }

    /// Whether this computer may keep a copy of `device`'s Bots.
    pub fn is_owner(&self, device: &DeviceId) -> bool {
        self.peers.get(device).is_some_and(|peer| peer.owner)
    }
}

/// The admission port. Cheap to clone; every clone reads the same answer.
#[derive(Clone)]
pub struct BotPeers {
    inner: Arc<Inner>,
}

struct Inner {
    current: RwLock<BotPeersSnapshot>,
    changed: tokio::sync::watch::Sender<u64>,
}

impl std::fmt::Debug for BotPeers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.current();
        formatter
            .debug_struct("BotPeers")
            .field("sequence", &current.sequence)
            .field("peers", &current.peers.len())
            .finish()
    }
}

impl Default for BotPeers {
    fn default() -> Self {
        Self::new()
    }
}

impl BotPeers {
    /// A port that has been told nothing.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                current: RwLock::new(BotPeersSnapshot::default()),
                changed: tokio::sync::watch::Sender::new(0),
            }),
        }
    }

    /// The answer now, from the snapshot this process holds.
    pub fn current(&self) -> BotPeersSnapshot {
        self.inner
            .current
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// A wake-up when the answer changes: the receiver's value is the new
    /// answer's sequence. It does not fire for a snapshot that changes
    /// nothing, such as the one a restarted Agent sends first.
    pub fn changed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    /// Take the Agent's authorization snapshot. In production the Store
    /// worker's sync control session is the only caller, beside the line
    /// that hands the same snapshot to replication.
    ///
    /// M8 fills the port from the own-device rule only: every own device is
    /// a client and an owner with `read`, `control` and `configure`, and no
    /// other peer is named.
    pub fn replace_authorization(&self, snapshot: &AuthorizationSnapshot) {
        let peers: BTreeMap<DeviceId, BotPeer> = snapshot
            .peers
            .iter()
            .filter(|peer| peer.own_device)
            .map(|peer| {
                (
                    peer.device_id,
                    BotPeer {
                        client: true,
                        owner: true,
                        scopes: [SCOPE_READ, SCOPE_CONTROL, SCOPE_CONFIGURE]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    },
                )
            })
            .collect();
        let sequence = {
            let mut current = self
                .inner
                .current
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if current.sequence != 0 && current.peers == peers {
                return;
            }
            current.sequence += 1;
            current.peers = peers;
            current.sequence
        };
        self.inner.changed.send_replace(sequence);
    }
}

#[cfg(test)]
mod tests {
    use koru_device::{RelationshipId, ReplicationEndpointId};
    use koru_sync_control::{AuthorizedPeer, ControlSessionId};

    use super::*;

    fn peer(device: DeviceId, own_device: bool) -> AuthorizedPeer {
        AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: device,
            endpoint_id: ReplicationEndpointId::from_bytes([3; 32]),
            // Replication scope is deliberately the opposite of the bit in
            // every fixture: the port reads `own_device` and nothing else.
            all_data: !own_device,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("a name the port never repeats".into()),
            build: None,
            own_device,
        }
    }

    fn snapshot(sequence: u64, peers: Vec<AuthorizedPeer>) -> AuthorizationSnapshot {
        AuthorizationSnapshot {
            session_id: ControlSessionId::new(),
            sequence,
            local_device: DeviceId::new(),
            peers,
            local_build: None,
        }
    }

    #[test]
    fn m8_bot_peers_port_names_nobody_until_it_is_told() {
        let port = BotPeers::new();
        let untold = port.current();
        assert_eq!(untold.sequence, 0);
        assert!(untold.peers.is_empty());
        assert_eq!(untold.client_scopes(&DeviceId::new()), None);
        assert!(!untold.is_owner(&DeviceId::new()));
        assert!(!port.changed().has_changed().unwrap());

        // Told that there is no linked device at all: that is an answer.
        port.replace_authorization(&snapshot(1, Vec::new()));
        let told = port.current();
        assert_eq!(told.sequence, 1);
        assert!(told.peers.is_empty());
    }

    #[test]
    fn m8_bot_peers_port_follows_replace_authorization() {
        let port = BotPeers::new();
        let own = DeviceId::new();
        let profile_only = DeviceId::new();
        let introduced = DeviceId::new();
        port.replace_authorization(&snapshot(
            1,
            vec![
                peer(own, true),
                peer(profile_only, false),
                peer(introduced, false),
            ],
        ));

        let answer = port.current();
        assert_eq!(answer.sequence, 1);
        // Every own device is a client and an owner with the three scopes of
        // M8, and no other peer is named.
        assert_eq!(answer.peers.keys().copied().collect::<Vec<_>>(), [own]);
        let named = &answer.peers[&own];
        assert!(named.client && named.owner);
        assert_eq!(
            named.scopes.iter().map(String::as_str).collect::<Vec<_>>(),
            ["configure", "control", "read"]
        );
        assert_eq!(answer.client_scopes(&own), Some(&named.scopes));
        assert!(answer.is_owner(&own));
        for other in [profile_only, introduced] {
            assert_eq!(answer.client_scopes(&other), None);
            assert!(!answer.is_owner(&other));
        }

        // The pairing is revoked: the next snapshot no longer names the
        // device, and neither does the port.
        port.replace_authorization(&snapshot(2, vec![peer(profile_only, false)]));
        let answer = port.current();
        assert_eq!(answer.sequence, 2);
        assert!(answer.peers.is_empty());
        assert_eq!(answer.client_scopes(&own), None);
        assert!(!answer.is_owner(&own));

        // The answer is the one every clone reads.
        assert_eq!(port.clone().current(), answer);
    }

    #[test]
    fn m8_bot_peers_port_changed_fires_when_the_answer_changes_and_only_then() {
        let port = BotPeers::new();
        let mut wake = port.changed();
        let own = DeviceId::new();
        assert!(!wake.has_changed().unwrap());

        port.replace_authorization(&snapshot(1, vec![peer(own, true)]));
        assert!(wake.has_changed().unwrap());
        assert_eq!(*wake.borrow_and_update(), 1);

        // A restarted Agent sends the same grant in a new control session,
        // with its sequence starting over and other cosmetics. Nothing
        // changed for Bots: no wake-up, and the port's sequence stays.
        let mut resent = snapshot(1, vec![peer(own, true), peer(DeviceId::new(), false)]);
        resent.peers[0].display_name = Some("renamed".into());
        port.replace_authorization(&resent);
        assert!(!wake.has_changed().unwrap());
        assert_eq!(port.current().sequence, 1);

        // A second own device.
        let second = DeviceId::new();
        port.replace_authorization(&snapshot(2, vec![peer(own, true), peer(second, true)]));
        assert!(wake.has_changed().unwrap());
        assert_eq!(*wake.borrow_and_update(), 2);
        assert_eq!(port.current().peers.len(), 2);

        // One stops being an own device.
        port.replace_authorization(&snapshot(3, vec![peer(own, false), peer(second, true)]));
        assert!(wake.has_changed().unwrap());
        assert_eq!(*wake.borrow_and_update(), 3);
        assert_eq!(
            port.current().peers.keys().copied().collect::<Vec<_>>(),
            [second]
        );

        // A subscriber that arrives later reads the current sequence.
        assert_eq!(*port.changed().borrow(), 3);
    }

    #[tokio::test]
    async fn m8_bot_peers_port_a_waiting_reader_is_woken() {
        let port = BotPeers::new();
        let mut wake = port.changed();
        let feeder = port.clone();
        let own = DeviceId::new();
        let waiting = tokio::spawn(async move {
            wake.changed().await.unwrap();
            *wake.borrow()
        });
        tokio::task::yield_now().await;
        feeder.replace_authorization(&snapshot(1, vec![peer(own, true)]));
        let sequence = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the reader was woken")
            .unwrap();
        assert_eq!(sequence, 1);
    }

    #[test]
    fn m8_bot_peers_port_debug_names_no_device() {
        let port = BotPeers::new();
        let own = DeviceId::new();
        port.replace_authorization(&snapshot(1, vec![peer(own, true)]));
        let printed = format!("{port:?}");
        assert!(!printed.contains(&own.to_storage()));
        assert!(!printed.contains("a name the port never repeats"));
    }
}
