//! Certificate updates: telling a peer this node's key has changed, without losing trust
//! (SHIP 1.1.0 §12.1.3).
//!
//! Trust is a pinned SKI and a SKI follows a key, so replacing a certificate would
//! otherwise break every trust relationship a device has. The way out is a transition
//! rather than a switch. Per curve a node has a **current** certificate, the one TLS is
//! using; optionally a **successor**, announced ahead of time; and optionally a
//! **predecessor**, just retired. An update announces the successor to trusted peers over
//! connections the current certificate still secures and lasts at least a month
//! ([`TRANSITION_STAGE`]); only then do the two move up.
//!
//! An `updateCounter` carries the whole state in one number, and rides in every `hello`,
//! so a peer that has been away notices at once that it is behind.
//!
//! ```
//! use eebus::ship::{KeyState, OwnKeys, Ski};
//!
//! let original: Ski = "1111111111111111111111111111111111111111".parse()?;
//! let renewed: Ski = "2222222222222222222222222222222222222222".parse()?;
//!
//! let mut keys = OwnKeys::new(original);
//! assert_eq!(keys.current(), Some(original));
//! assert_eq!(keys.update_counter(), 1);
//!
//! // The month-long transition: peers learn the new SKI while the old one still works.
//! keys.begin_update(renewed)?;
//! assert_eq!(keys.current(), Some(original), "TLS still uses the old one");
//! assert_eq!(keys.state_of(&renewed), Some(KeyState::Successor));
//! assert_eq!(keys.update_counter(), 2, "and peers can see something changed");
//!
//! // Afterwards the successor takes over and the old key is retired.
//! keys.complete_update();
//! assert_eq!(keys.current(), Some(renewed));
//! assert_eq!(keys.state_of(&original), Some(KeyState::Predecessor));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use super::ski::Ski;
use super::{KeyMaterialStateCurrentState, KeyMaterialStateEntry};

/// The IANA "TLS Supported Groups" number of secp256r1, which SHIP §9.3 makes a SHALL.
///
/// The `curve` element of a key-material entry is this registry's number, so that a node
/// supporting more than one curve can say which certificate belongs to which.
pub const CURVE_SECP256R1: u16 = 23;

/// How long a successor is announced before it replaces the current certificate.
///
/// SHIP §12.1.3.1 sets the floor at one month and suggests a quarter of the certificate's
/// lifetime. The point of the wait is that a peer which is switched off for a fortnight
/// still learns the new key before the old one stops working.
pub const TRANSITION_STAGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How long to wait for a `keyMaterialStateResponse` before sending the state again.
///
/// §12.1.3.2: resend after thirty seconds; if the second attempt is also unanswered,
/// close the connection and try again on a fresh one.
pub const STATE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a node has to answer a `keyMaterialState` (§12.1.3.3).
pub const RESPONSE_DEADLINE: Duration = Duration::from_secs(20);

/// How often a node may dial a peer for the sole purpose of updating key material.
pub const UPDATE_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Which of a node's certificates an entry describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// Retired. A peer that still trusts it SHALL stop.
    Predecessor,
    /// In use by TLS now.
    Current,
    /// Announced, not yet in use.
    Successor,
}

impl KeyState {
    fn to_model(self) -> KeyMaterialStateCurrentState {
        match self {
            KeyState::Predecessor => KeyMaterialStateCurrentState::Predecessor,
            KeyState::Current => KeyMaterialStateCurrentState::Current,
            KeyState::Successor => KeyMaterialStateCurrentState::Successor,
        }
    }

    fn from_model(state: &KeyMaterialStateCurrentState) -> Self {
        match state {
            KeyMaterialStateCurrentState::Predecessor => KeyState::Predecessor,
            KeyMaterialStateCurrentState::Current => KeyState::Current,
            KeyMaterialStateCurrentState::Successor => KeyState::Successor,
        }
    }
}

/// One certificate of one curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEntry {
    /// Which certificate this is.
    pub state: KeyState,
    /// The curve, by its IANA number.
    pub curve: u16,
    /// The Subject Key Identifier, which is what a peer pins.
    pub ski: Ski,
}

/// Why a key-material change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// An update is already running; finish it before starting another.
    #[error("a certificate update is already in progress")]
    UpdateInProgress,
    /// The successor is the certificate already in use.
    #[error("the successor is the current certificate")]
    NotAChange,
    /// There is no current certificate for that curve to succeed.
    #[error("no current certificate for that curve")]
    UnknownCurve,
}

/// This node's own key material, and the state of any update in progress.
///
/// Named for the specification's own distinction (§3): key material is "own" when it
/// belongs to this node's certificates and "foreign" when it belongs to a peer's — see
/// [`PeerKeys`] for the other half.
#[derive(Clone, Debug)]
pub struct OwnKeys {
    update_counter: u16,
    entries: Vec<KeyEntry>,
}

impl OwnKeys {
    /// Factory-new: one current certificate on secp256r1, and no history.
    pub fn new(current: Ski) -> Self {
        Self::for_curve(current, CURVE_SECP256R1)
    }

    /// Factory-new on a particular curve.
    pub fn for_curve(current: Ski, curve: u16) -> Self {
        Self {
            // Counters start at one, so that a peer with nothing stored — which reads as
            // zero — sees the first announcement as an increase.
            update_counter: 1,
            entries: alloc::vec![KeyEntry {
                state: KeyState::Current,
                curve,
                ski: current,
            }],
        }
    }

    /// The `updateCounter` a peer compares against what it has stored.
    pub fn update_counter(&self) -> u16 {
        self.update_counter
    }

    /// Every certificate held.
    pub fn entries(&self) -> &[KeyEntry] {
        &self.entries
    }

    /// The certificate TLS is using, on secp256r1.
    pub fn current(&self) -> Option<Ski> {
        self.current_for(CURVE_SECP256R1)
    }

    /// The certificate TLS is using on a given curve.
    pub fn current_for(&self, curve: u16) -> Option<Ski> {
        self.find(KeyState::Current, curve).map(|e| e.ski)
    }

    /// The announced successor on a given curve, if an update is running.
    pub fn successor_for(&self, curve: u16) -> Option<Ski> {
        self.find(KeyState::Successor, curve).map(|e| e.ski)
    }

    /// Whether a certificate update is in its transition stage.
    pub fn update_in_progress(&self) -> bool {
        self.entries.iter().any(|e| e.state == KeyState::Successor)
    }

    /// What this node calls one of its own SKIs.
    pub fn state_of(&self, ski: &Ski) -> Option<KeyState> {
        self.entries.iter().find(|e| &e.ski == ski).map(|e| e.state)
    }

    /// Announces a successor, starting the transition stage.
    ///
    /// TLS keeps using the current certificate throughout: the point of the stage is that
    /// peers learn the new SKI over connections the old one still secures. Add a curve
    /// that has no current certificate with [`add_curve`](Self::add_curve) instead —
    /// §12.1.3.1 permits that at any time, because a new curve breaks nothing.
    pub fn begin_update(&mut self, successor: Ski) -> Result<(), KeyError> {
        self.begin_update_for(successor, CURVE_SECP256R1)
    }

    /// Announces a successor on a particular curve.
    pub fn begin_update_for(&mut self, successor: Ski, curve: u16) -> Result<(), KeyError> {
        if self.update_in_progress() {
            return Err(KeyError::UpdateInProgress);
        }
        let Some(current) = self.find(KeyState::Current, curve) else {
            return Err(KeyError::UnknownCurve);
        };
        if current.ski == successor {
            return Err(KeyError::NotAChange);
        }
        self.entries.push(KeyEntry {
            state: KeyState::Successor,
            curve,
            ski: successor,
        });
        self.bump();
        Ok(())
    }

    /// Ends the transition stage: the successor takes over and the old key is retired.
    ///
    /// Call it once [`TRANSITION_STAGE`] has passed since [`begin_update`](Self::begin_update).
    /// Afterwards the secp256r1 SKI in the `_ship._tcp` TXT record has to be updated too
    /// (§7.3.2), or a peer looking for this node by SKI will not find it.
    ///
    /// Returns whether anything changed.
    pub fn complete_update(&mut self) -> bool {
        if !self.update_in_progress() {
            return false;
        }
        // A curve keeps at most one predecessor: the one before last is of no use to
        // anybody, and §12.1.3.3 has peers untrusting it in any case.
        let succeeded: Vec<u16> = self
            .entries
            .iter()
            .filter(|e| e.state == KeyState::Successor)
            .map(|e| e.curve)
            .collect();
        self.entries
            .retain(|e| !(e.state == KeyState::Predecessor && succeeded.contains(&e.curve)));
        for entry in &mut self.entries {
            if !succeeded.contains(&entry.curve) {
                continue;
            }
            entry.state = match entry.state {
                KeyState::Current => KeyState::Predecessor,
                KeyState::Successor => KeyState::Current,
                other => other,
            };
        }
        self.bump();
        true
    }

    /// Adds a certificate on a curve this node did not support before.
    ///
    /// §12.1.3.1 allows this without a transition stage: an added curve takes nothing
    /// away, and a peer that does not want it simply carries on with the one it has.
    pub fn add_curve(&mut self, ski: Ski, curve: u16) -> Result<(), KeyError> {
        if self.find(KeyState::Current, curve).is_some() {
            return Err(KeyError::UpdateInProgress);
        }
        self.entries.push(KeyEntry {
            state: KeyState::Current,
            curve,
            ski,
        });
        self.bump();
        Ok(())
    }

    /// Drops a curve entirely.
    ///
    /// §12.1.3.1 counts this as a certificate update: after it, `keyMaterialState`
    /// carries nothing for that curve, and a peer that trusted this node only there loses
    /// the relationship.
    pub fn drop_curve(&mut self, curve: u16) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.curve != curve);
        if self.entries.len() == before {
            return false;
        }
        self.bump();
        true
    }

    /// The `keyMaterialState` message announcing all of this.
    ///
    /// §12.1.3.2: every certificate held goes in exactly one message, and no foreign SKI
    /// ever does.
    pub fn to_message(&self) -> super::KeyMaterialState {
        super::KeyMaterialState {
            update_counter: Some(self.update_counter),
            entry: Some(
                self.entries
                    .iter()
                    .map(|entry| KeyMaterialStateEntry {
                        current_state: Some(entry.state.to_model()),
                        curve: Some(entry.curve),
                        ski: Some(entry.ski.to_txt_value()),
                        public_key: None,
                    })
                    .collect(),
            ),
        }
    }

    fn find(&self, state: KeyState, curve: u16) -> Option<&KeyEntry> {
        self.entries
            .iter()
            .find(|e| e.state == state && e.curve == curve)
    }

    fn bump(&mut self) {
        // §12.1.3.4: a counter that went down is not trustworthy, so it never does — it
        // saturates instead, and a node that has renewed sixty-five thousand times has
        // other problems.
        self.update_counter = self.update_counter.saturating_add(1);
    }
}

/// What changed about a peer's key material.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyUpdate {
    /// SKIs now to be trusted for this peer.
    pub trust: Vec<Ski>,
    /// SKIs no longer to be trusted: retired, or simply gone from the announcement.
    pub untrust: Vec<Ski>,
    /// Whether the announcement was newer than what was held.
    pub advanced: bool,
}

/// What this node knows about one peer's key material.
///
/// Kept per peer, and persisted with the trust store: the whole point is that a device
/// switched off for a month still recognises the peer when it comes back.
#[derive(Clone, Debug, Default)]
pub struct PeerKeys {
    update_counter: u16,
    trusted: Vec<(u16, Ski)>,
}

impl PeerKeys {
    /// Nothing known yet: counter zero, nothing trusted through this mechanism.
    pub fn new() -> Self {
        Self::default()
    }

    /// What was stored when this peer was last heard from.
    pub fn from_stored(update_counter: u16, trusted: impl IntoIterator<Item = (u16, Ski)>) -> Self {
        Self {
            update_counter,
            trusted: trusted.into_iter().collect(),
        }
    }

    /// The `updateCounter` last accepted from this peer.
    pub fn update_counter(&self) -> u16 {
        self.update_counter
    }

    /// The SKIs trusted for this peer, with the curve each belongs to.
    pub fn trusted(&self) -> &[(u16, Ski)] {
        &self.trusted
    }

    /// Whether an announcement seen in the `hello` phase means this node is behind.
    ///
    /// §12.1.3.4: a higher counter in `hello` is the signal to ask for the rest with a
    /// `keyMaterialStateRequest` once data exchange opens.
    pub fn is_outdated_by(&self, announced: u16) -> bool {
        announced > self.update_counter
    }

    /// Applies a peer's `keyMaterialState`, and says what to do about it.
    ///
    /// `negotiated_curve` is the curve the TLS connection this arrived on is using. Only
    /// the peer's certificates on *that* curve are taken up: §12.1.3.3 says SKIs of other
    /// curves SHOULD NOT be trusted, because trusting a curve nobody has verified widens
    /// the attack surface for no gain.
    ///
    /// Returns [`None`] when the counter has gone *backwards*, which is what a replay
    /// looks like and which §12.1.3.4 does not treat as trustworthy.
    ///
    /// An announcement at the counter already held is applied, with
    /// [`KeyUpdate::advanced`] false: a node that reconnects and restates an unchanged
    /// key set has to be able to re-synchronise a peer that lost its store, and the
    /// counter is a staleness hint rather than an access control — the message arrives
    /// over a connection the peer's current certificate already authenticated.
    pub fn apply(
        &mut self,
        message: &super::KeyMaterialState,
        negotiated_curve: u16,
    ) -> Option<KeyUpdate> {
        let announced = message.update_counter?;
        if announced < self.update_counter {
            return None;
        }

        let mut update = KeyUpdate {
            advanced: announced > self.update_counter,
            ..KeyUpdate::default()
        };

        let mut keep: Vec<(u16, Ski)> = Vec::new();
        for entry in message.entry.iter().flatten() {
            let (Some(state), Some(curve), Some(ski)) = (
                entry.current_state.as_ref().map(KeyState::from_model),
                entry.curve,
                entry.ski.as_deref().and_then(|s| s.parse::<Ski>().ok()),
            ) else {
                continue;
            };
            if curve != negotiated_curve {
                continue;
            }
            match state {
                // §12.1.3.3: current and successor are trusted; a predecessor is not.
                KeyState::Current | KeyState::Successor => keep.push((curve, ski)),
                KeyState::Predecessor => {}
            }
        }

        for (curve, ski) in &keep {
            if !self.trusted.contains(&(*curve, *ski)) {
                update.trust.push(*ski);
            }
        }
        // "A SHIP node SHALL NOT trust foreign key material of a SHIP node that is not
        // contained in the received keyMaterialState anymore." Only for the curve this
        // message speaks for: it says nothing about the others.
        for (curve, ski) in &self.trusted {
            if *curve == negotiated_curve && !keep.contains(&(*curve, *ski)) {
                update.untrust.push(*ski);
            }
        }

        self.trusted.retain(|(curve, _)| *curve != negotiated_curve);
        self.trusted.extend(keep);
        self.update_counter = announced;
        Some(update)
    }
}

/// Reads a SKI out of a key-material entry.
///
/// The hex is uppercase on the wire from SHIP 1.1.0 onwards, and lowercase from peers
/// that predate it; both parse.
pub fn entry_ski(entry: &KeyMaterialStateEntry) -> Option<Ski> {
    entry.ski.as_deref()?.parse().ok()
}

/// A SKI as SHIP 1.1.0 writes it: forty uppercase hex digits.
pub fn ski_text(ski: &Ski) -> String {
    ski.to_txt_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ski(byte: u8) -> Ski {
        Ski::from_bytes([byte; 20])
    }

    /// §12.1.3.2: the transition announces a successor while the current key stays in use.
    #[test]
    fn a_transition_announces_before_it_switches() {
        let mut keys = OwnKeys::new(ski(1));
        assert!(!keys.update_in_progress());

        keys.begin_update(ski(2)).unwrap();
        assert!(keys.update_in_progress());
        assert_eq!(keys.current(), Some(ski(1)), "TLS still uses the old key");
        assert_eq!(keys.successor_for(CURVE_SECP256R1), Some(ski(2)));

        assert!(keys.complete_update());
        assert_eq!(keys.current(), Some(ski(2)));
        assert_eq!(keys.state_of(&ski(1)), Some(KeyState::Predecessor));
        assert!(!keys.update_in_progress());
    }

    /// The counter only ever goes up, because §12.1.3.4 treats a decrease as untrustworthy.
    #[test]
    fn the_update_counter_only_advances() {
        let mut keys = OwnKeys::new(ski(1));
        let start = keys.update_counter();
        keys.begin_update(ski(2)).unwrap();
        keys.complete_update();
        keys.begin_update(ski(3)).unwrap();
        assert!(keys.update_counter() > start + 1);
    }

    #[test]
    fn a_second_update_before_the_first_finishes_is_refused() {
        let mut keys = OwnKeys::new(ski(1));
        keys.begin_update(ski(2)).unwrap();
        assert_eq!(keys.begin_update(ski(3)), Err(KeyError::UpdateInProgress));
        assert_eq!(keys.begin_update(ski(2)), Err(KeyError::UpdateInProgress));
    }

    #[test]
    fn succeeding_a_key_with_itself_is_not_an_update() {
        let mut keys = OwnKeys::new(ski(1));
        assert_eq!(keys.begin_update(ski(1)), Err(KeyError::NotAChange));
    }

    /// A second renewal does not leave the first predecessor lying around.
    #[test]
    fn only_one_predecessor_is_kept_per_curve() {
        let mut keys = OwnKeys::new(ski(1));
        keys.begin_update(ski(2)).unwrap();
        keys.complete_update();
        keys.begin_update(ski(3)).unwrap();
        keys.complete_update();

        assert_eq!(keys.current(), Some(ski(3)));
        assert_eq!(keys.state_of(&ski(2)), Some(KeyState::Predecessor));
        assert_eq!(keys.state_of(&ski(1)), None, "the oldest is gone");
    }

    /// §12.1.3.2: everything held goes in one message, and no foreign SKI ever does.
    #[test]
    fn the_message_carries_every_certificate_and_the_counter() {
        let mut keys = OwnKeys::new(ski(1));
        keys.begin_update(ski(2)).unwrap();

        let message = keys.to_message();
        assert_eq!(message.update_counter, Some(keys.update_counter()));
        let entries = message.entry.as_ref().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.curve == Some(CURVE_SECP256R1)));
        assert!(
            entries
                .iter()
                .any(|e| e.current_state == Some(KeyMaterialStateCurrentState::Successor))
        );
    }

    /// §12.1.3.3: current and successor are trusted; a predecessor is untrusted.
    #[test]
    fn a_peer_takes_up_the_new_key_and_drops_the_retired_one() {
        let mut peer = PeerKeys::new();
        let mut theirs = OwnKeys::new(ski(1));

        let update = peer
            .apply(&theirs.to_message(), CURVE_SECP256R1)
            .expect("a first announcement is news");
        assert_eq!(update.trust, alloc::vec![ski(1)]);
        assert!(update.untrust.is_empty());

        theirs.begin_update(ski(2)).unwrap();
        let update = peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();
        assert_eq!(
            update.trust,
            alloc::vec![ski(2)],
            "the successor is trusted early"
        );
        assert!(update.untrust.is_empty(), "the current key still works");

        theirs.complete_update();
        let update = peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();
        assert!(update.trust.is_empty(), "nothing new");
        assert_eq!(
            update.untrust,
            alloc::vec![ski(1)],
            "the predecessor is dropped"
        );
        assert_eq!(peer.trusted(), &[(CURVE_SECP256R1, ski(2))]);
    }

    /// §12.1.3.3: a certificate for a curve this connection is not using is not taken up.
    #[test]
    fn a_key_for_another_curve_is_not_trusted() {
        const BRAINPOOL_P256R1: u16 = 26;

        let mut theirs = OwnKeys::new(ski(1));
        theirs.add_curve(ski(9), BRAINPOOL_P256R1).unwrap();

        let mut peer = PeerKeys::new();
        let update = peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();
        assert_eq!(
            update.trust,
            alloc::vec![ski(1)],
            "only the negotiated curve's key"
        );
        assert_eq!(peer.trusted().len(), 1);
    }

    /// §12.1.3.4: a counter that went backwards is what a replay looks like.
    #[test]
    fn an_older_announcement_is_refused() {
        let mut theirs = OwnKeys::new(ski(1));
        theirs.begin_update(ski(2)).unwrap();
        let newer = theirs.to_message();

        let mut peer = PeerKeys::new();
        peer.apply(&newer, CURVE_SECP256R1).unwrap();

        let stale = OwnKeys::new(ski(3)).to_message();
        assert!(
            peer.apply(&stale, CURVE_SECP256R1).is_none(),
            "a replayed announcement is not a certificate update"
        );
        assert!(!peer.trusted().iter().any(|(_, s)| *s == ski(3)));
    }

    /// The `hello` counter is what tells a returning peer it has fallen behind.
    #[test]
    fn a_higher_counter_in_hello_means_there_is_something_to_ask_for() {
        let mut theirs = OwnKeys::new(ski(1));
        let mut peer = PeerKeys::new();
        peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();

        assert!(!peer.is_outdated_by(theirs.update_counter()));
        theirs.begin_update(ski(2)).unwrap();
        assert!(peer.is_outdated_by(theirs.update_counter()));
    }

    /// Dropping a curve is a certificate update, and takes the trust with it.
    #[test]
    fn dropping_a_curve_untrusts_what_was_on_it() {
        let mut theirs = OwnKeys::new(ski(1));
        let mut peer = PeerKeys::new();
        peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();

        assert!(theirs.drop_curve(CURVE_SECP256R1));
        let update = peer.apply(&theirs.to_message(), CURVE_SECP256R1).unwrap();
        assert_eq!(update.untrust, alloc::vec![ski(1)]);
        assert!(peer.trusted().is_empty());
    }
}
