//! Bindings and subscriptions: who may write, and who gets told.
//!
//! Both are relations between a client feature and a server feature, requested by the
//! client and granted or refused by the server (Protocol Specification §7.3, §7.4).
//! They answer different questions:
//!
//! * a **binding** grants permission to write. A write without one is refused with
//!   `errorNumber` 9, which is what stops a second energy manager from overriding the
//!   first one's limit.
//! * a **subscription** asks to be notified when data changes, so that a client does not
//!   have to poll. The use-case implementation guide §3.2.2 makes subscriptions the
//!   primary mechanism and polling an error-handling fallback.
//!
//! Both are kept on the primary NodeManagement instance, and both are dropped when the
//! connection to the peer they belong to goes away.

use alloc::vec::Vec;

use crate::model::{BindingId, FeatureAddress, SubscriptionId};

use super::ack::ErrorNumber;
use super::address::{is_node_management, same_entity, same_feature};

/// How many clients may bind to one server feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BindingPolicy {
    /// One binding partner per feature, and the first to ask keeps it.
    ///
    /// The default, because SPINE 1.4.0 makes it the rule and the LPC implementation
    /// guide §3.5 asks implementers to apply it already. Without it, two energy managers
    /// can each hold a binding on the same `LoadControl` feature and fight over the
    /// limit, which SPINE gives no way to resolve.
    ///
    /// Features declared with [`Relations::bind_together`] share one partner rather than
    /// holding one each.
    #[default]
    SinglePerFeature,
    /// Several clients may bind to the same feature.
    ///
    /// Correct where a feature carries entries belonging to different use cases, each
    /// controlled by a different peer; the server is then responsible for keeping them
    /// apart (LPC implementation guide §3.5).
    MultiplePerFeature,
}

/// How many bindings, and how many subscriptions, one peer device may hold on this node.
///
/// A relation is memory the *peer* allocates: it asks, and the entry lives until it or the
/// connection goes away. SPINE caps neither, and a peer that has completed a SHIP handshake
/// can ask from any client address it likes — a fresh entity path per request — so without
/// a bound the two tables grow on the word of whoever is on the wire. Thirty-two of each
/// is far beyond what a use case needs: the Energy Guard of LPC holds two bindings and
/// three subscriptions, and an energy manager watching a large inverter a dozen. Beyond it
/// the answer is `errorNumber` 3 — overload — which is what §5.2.5 defines it for.
pub const MAX_RELATIONS_PER_PEER: usize = 32;

/// One binding or subscription.
#[derive(Clone, Debug, PartialEq)]
pub struct Relation {
    /// The identifier the server assigned.
    pub id: u32,
    /// The client feature that asked.
    pub client: FeatureAddress,
    /// The server feature it relates to.
    pub server: FeatureAddress,
}

/// The bindings and subscriptions a local device holds.
#[derive(Clone, Debug)]
pub struct Relations {
    bindings: Vec<Relation>,
    subscriptions: Vec<Relation>,
    groups: Vec<Vec<FeatureAddress>>,
    next_binding: u32,
    next_subscription: u32,
    policy: BindingPolicy,
}

impl Default for Relations {
    fn default() -> Self {
        Self::new(BindingPolicy::default())
    }
}

impl Relations {
    /// An empty set of relations.
    pub fn new(policy: BindingPolicy) -> Self {
        Self {
            bindings: Vec::new(),
            subscriptions: Vec::new(),
            groups: Vec::new(),
            next_binding: 1,
            next_subscription: 1,
            policy,
        }
    }

    /// Declares that these features belong to one use case and share a binding partner.
    ///
    /// The LPC implementation guide §3.5 (version 1.1.0) describes the race this closes.
    /// LPC needs a binding on `LoadControl` *and* on `DeviceConfiguration`; with a
    /// per-feature lock alone, two energy managers coming up together can each win one
    /// of them, after which neither can run the use case and neither can recover. Locking
    /// the group means the first device to bind any of them holds all of them.
    pub fn bind_together(&mut self, features: impl IntoIterator<Item = FeatureAddress>) {
        let group: Vec<FeatureAddress> = features.into_iter().collect();
        if group.len() > 1 {
            self.groups.push(group);
        }
    }

    /// The features that share a binding partner with `server`, `server` included.
    fn group_of<'a>(&'a self, server: &'a FeatureAddress) -> &'a [FeatureAddress] {
        self.groups
            .iter()
            .find(|group| group.iter().any(|f| same_feature(f, server)))
            .map_or(core::slice::from_ref(server), Vec::as_slice)
    }

    /// The binding policy in force.
    pub fn policy(&self) -> BindingPolicy {
        self.policy
    }

    /// Replaces the binding policy.
    pub fn set_policy(&mut self, policy: BindingPolicy) {
        self.policy = policy;
    }

    /// The bindings held.
    pub fn bindings(&self) -> &[Relation] {
        &self.bindings
    }

    /// The subscriptions held.
    pub fn subscriptions(&self) -> &[Relation] {
        &self.subscriptions
    }

    /// Grants a binding, or says why not.
    ///
    /// ```
    /// use eebus::spine::{device_address, feature_address, Relations};
    ///
    /// let client = feature_address(&device_address("i:1", "ControlBox").unwrap(), &[1], 1);
    /// let server = feature_address(&device_address("i:2", "HeatPump").unwrap(), &[1], 1);
    ///
    /// let mut relations = Relations::default();
    /// let id = relations.add_binding(&client, &server).unwrap();
    /// assert!(relations.is_bound(&client, &server));
    ///
    /// // Asking again returns the binding that already exists rather than a second one.
    /// assert_eq!(relations.add_binding(&client, &server), Ok(id));
    /// ```
    pub fn add_binding(
        &mut self,
        client: &FeatureAddress,
        server: &FeatureAddress,
    ) -> Result<BindingId, ErrorNumber> {
        // `TC_SPINE_BIND_001`: NodeManagement carries the binding machinery itself and
        // is never a binding target.
        if is_node_management(server) {
            return Err(ErrorNumber::CommandRejected);
        }

        if let Some(existing) = self
            .bindings
            .iter()
            .find(|b| same_feature(&b.client, client) && same_feature(&b.server, server))
        {
            return Ok(BindingId(existing.id));
        }

        if self.policy == BindingPolicy::SinglePerFeature {
            let group = self.group_of(server);
            let taken = self.bindings.iter().any(|b| {
                group.iter().any(|f| same_feature(&b.server, f)) && !same_entity(&b.client, client)
            });
            if taken {
                return Err(ErrorNumber::CommandRejected);
            }
        }
        if held_by_peer(&self.bindings, client) >= MAX_RELATIONS_PER_PEER {
            return Err(ErrorNumber::Overload);
        }

        let id = self.next_binding;
        self.next_binding += 1;
        self.bindings.push(Relation {
            id,
            client: client.clone(),
            server: server.clone(),
        });
        Ok(BindingId(id))
    }

    /// Releases a binding. Either partner may (Protocol Specification §7.3, rule 6).
    ///
    /// Releasing one that does not exist succeeds: the caller's intent is already met,
    /// and a peer that retries a delete after a reconnection should not see an error.
    pub fn remove_binding(&mut self, client: &FeatureAddress, server: &FeatureAddress) {
        self.bindings
            .retain(|b| !(same_feature(&b.client, client) && same_feature(&b.server, server)));
    }

    /// Whether a client may write to a server feature.
    pub fn is_bound(&self, client: &FeatureAddress, server: &FeatureAddress) -> bool {
        self.bindings
            .iter()
            .any(|b| same_feature(&b.client, client) && same_feature(&b.server, server))
    }

    /// Grants a subscription, or says why not.
    pub fn add_subscription(
        &mut self,
        client: &FeatureAddress,
        server: &FeatureAddress,
    ) -> Result<SubscriptionId, ErrorNumber> {
        if let Some(existing) = self
            .subscriptions
            .iter()
            .find(|s| same_feature(&s.client, client) && same_feature(&s.server, server))
        {
            return Ok(SubscriptionId(existing.id));
        }
        if held_by_peer(&self.subscriptions, client) >= MAX_RELATIONS_PER_PEER {
            return Err(ErrorNumber::Overload);
        }
        let id = self.next_subscription;
        self.next_subscription += 1;
        self.subscriptions.push(Relation {
            id,
            client: client.clone(),
            server: server.clone(),
        });
        Ok(SubscriptionId(id))
    }

    /// Releases a subscription.
    ///
    /// `TC_SPINE_SUBS_002` requires this to be idempotent: deleting one that is not
    /// there is not an error.
    pub fn remove_subscription(&mut self, client: &FeatureAddress, server: &FeatureAddress) {
        self.subscriptions
            .retain(|s| !(same_feature(&s.client, client) && same_feature(&s.server, server)));
    }

    /// Whether a client is subscribed to a server feature.
    pub fn is_subscribed(&self, client: &FeatureAddress, server: &FeatureAddress) -> bool {
        self.subscriptions
            .iter()
            .any(|s| same_feature(&s.client, client) && same_feature(&s.server, server))
    }

    /// The clients to notify when a server feature's data changes.
    pub fn subscribers_of<'a>(
        &'a self,
        server: &'a FeatureAddress,
    ) -> impl Iterator<Item = &'a FeatureAddress> + 'a {
        self.subscriptions
            .iter()
            .filter(move |s| same_feature(&s.server, server))
            .map(|s| &s.client)
    }

    /// Drops everything belonging to a peer.
    ///
    /// Called when a connection ends. The LPC implementation guide §2.17 is worth
    /// remembering here: discarding these is right, but it says nothing about the use
    /// case's own state, which survives a reconnection.
    pub fn remove_device(&mut self, device: &crate::model::AddressDevice) {
        let belongs = |address: &FeatureAddress| address.device.as_ref() == Some(device);
        self.bindings
            .retain(|b| !belongs(&b.client) && !belongs(&b.server));
        self.subscriptions
            .retain(|s| !belongs(&s.client) && !belongs(&s.server));
    }
}

/// How many of `relations` a peer device holds — the same device as `client`, whichever
/// of its features asked.
fn held_by_peer(relations: &[Relation], client: &FeatureAddress) -> usize {
    relations
        .iter()
        .filter(|relation| relation.client.device == client.device)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::{device_address, feature_address, node_management};

    /// Nothing a peer can grow is unbounded: a peer that keeps asking from fresh client
    /// addresses is told it has had enough, rather than served until the memory runs out.
    #[test]
    fn a_peer_cannot_hold_more_relations_than_the_cap() {
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        let cem = device_address("i:12345", "Cem").unwrap();
        let mut relations = Relations::new(BindingPolicy::MultiplePerFeature);

        for entity in 1..=MAX_RELATIONS_PER_PEER as u32 {
            let client = feature_address(&cem, &[entity], 1);
            relations
                .add_subscription(&client, &feature_address(&heat_pump, &[1], 1))
                .expect("within the cap");
            relations
                .add_binding(&client, &feature_address(&heat_pump, &[1], 1))
                .expect("within the cap");
        }
        let one_more = feature_address(&cem, &[99], 1);
        let server = feature_address(&heat_pump, &[1], 1);
        assert_eq!(
            relations.add_subscription(&one_more, &server),
            Err(ErrorNumber::Overload)
        );
        assert_eq!(
            relations.add_binding(&one_more, &server),
            Err(ErrorNumber::Overload)
        );

        // The cap is per peer: another device starts from nothing.
        let other = feature_address(&device_address("i:99999", "Other").unwrap(), &[1], 1);
        assert!(relations.add_subscription(&other, &server).is_ok());

        // And a relation that already exists is answered with its identifier, not refused.
        let first = feature_address(&cem, &[1], 1);
        assert!(relations.add_subscription(&first, &server).is_ok());
    }

    fn addresses() -> (FeatureAddress, FeatureAddress) {
        let control_box = device_address("i:12345", "ControlBox").unwrap();
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        (
            feature_address(&control_box, &[1], 1),
            feature_address(&heat_pump, &[1], 1),
        )
    }

    /// `TC_SPINE_BIND_001`: NodeManagement refuses to be bound to.
    #[test]
    fn tc_spine_bind_001_node_management_denies_bindings() {
        let (client, _) = addresses();
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        let mut relations = Relations::default();
        assert_eq!(
            relations.add_binding(&client, &node_management(&heat_pump)),
            Err(ErrorNumber::CommandRejected)
        );
    }

    /// `TC_SPINE_SUBS_001`: NodeManagement accepts subscriptions — that is how a peer
    /// learns of changes to the discovery data itself.
    #[test]
    fn tc_spine_subs_001_node_management_accepts_subscriptions() {
        let (client, _) = addresses();
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        let mut relations = Relations::default();
        assert!(
            relations
                .add_subscription(&client, &node_management(&heat_pump))
                .is_ok()
        );
    }

    /// `TC_SPINE_SUBS_002`: deleting a subscription that is not there is not an error.
    #[test]
    fn tc_spine_subs_002_subscription_deletion_is_idempotent() {
        let (client, server) = addresses();
        let mut relations = Relations::default();
        relations.remove_subscription(&client, &server);
        relations.add_subscription(&client, &server).unwrap();
        relations.remove_subscription(&client, &server);
        relations.remove_subscription(&client, &server);
        assert!(!relations.is_subscribed(&client, &server));
    }

    /// SPINE 1.4.0 and LPC implementation guide §3.5: one binding partner per feature.
    #[test]
    fn a_second_client_cannot_bind_to_the_same_feature() {
        let (client, server) = addresses();
        let other = feature_address(&device_address("i:99999", "OtherCem").unwrap(), &[1], 1);

        let mut relations = Relations::default();
        relations.add_binding(&client, &server).unwrap();
        assert_eq!(
            relations.add_binding(&other, &server),
            Err(ErrorNumber::CommandRejected),
            "the first binder keeps the feature"
        );
        assert!(relations.is_bound(&client, &server));
        assert!(!relations.is_bound(&other, &server));
    }

    /// LPC implementation guide §3.5 (1.1.0): features one use case needs together are
    /// locked to one partner, so two energy managers racing cannot win one each.
    #[test]
    fn features_bound_together_go_to_one_partner() {
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        let load_control = feature_address(&heat_pump, &[1], 1);
        let configuration = feature_address(&heat_pump, &[1], 2);

        let first = feature_address(&device_address("i:12345", "CemA").unwrap(), &[1], 1);
        let second = feature_address(&device_address("i:99999", "CemB").unwrap(), &[1], 1);

        let mut relations = Relations::default();
        relations.bind_together([load_control.clone(), configuration.clone()]);

        // The race the guide describes: A takes LoadControl, B goes for
        // DeviceConfiguration a moment later.
        relations.add_binding(&first, &load_control).unwrap();
        assert_eq!(
            relations.add_binding(&second, &configuration),
            Err(ErrorNumber::CommandRejected),
            "the group is A's now"
        );
        assert!(relations.add_binding(&first, &configuration).is_ok());
    }

    /// §3.8: the two bindings may come from different features of the same entity —
    /// which feature an actor writes from is its own business.
    #[test]
    fn one_entity_may_bind_a_group_from_two_of_its_features() {
        let heat_pump = device_address("i:67890", "HeatPump").unwrap();
        let load_control = feature_address(&heat_pump, &[1], 1);
        let configuration = feature_address(&heat_pump, &[1], 2);

        let cem = device_address("i:12345", "Cem").unwrap();
        let generic = feature_address(&cem, &[1], 1);
        let other = feature_address(&cem, &[1], 7);
        let elsewhere = feature_address(&cem, &[2], 1);

        let mut relations = Relations::default();
        relations.bind_together([load_control.clone(), configuration.clone()]);
        relations.add_binding(&generic, &load_control).unwrap();
        assert!(relations.add_binding(&other, &configuration).is_ok());
        assert_eq!(
            relations.add_binding(&elsewhere, &configuration),
            Err(ErrorNumber::CommandRejected),
            "a second entity of the same device is still a second entity"
        );
    }

    /// Where a feature carries entries for several use cases, the server may allow more
    /// than one binding and keep them apart itself.
    #[test]
    fn the_policy_can_allow_several_binding_partners() {
        let (client, server) = addresses();
        let other = feature_address(&device_address("i:99999", "OtherCem").unwrap(), &[1], 1);

        let mut relations = Relations::new(BindingPolicy::MultiplePerFeature);
        relations.add_binding(&client, &server).unwrap();
        assert!(relations.add_binding(&other, &server).is_ok());
        assert_eq!(relations.bindings().len(), 2);
    }

    #[test]
    fn identifiers_are_assigned_once_and_reused_on_a_repeat_request() {
        let (client, server) = addresses();
        let mut relations = Relations::default();
        let first = relations.add_binding(&client, &server).unwrap();
        assert_eq!(relations.add_binding(&client, &server), Ok(first));
        assert_eq!(relations.bindings().len(), 1);

        let subscription = relations.add_subscription(&client, &server).unwrap();
        assert_eq!(
            relations.add_subscription(&client, &server),
            Ok(subscription)
        );
    }

    #[test]
    fn subscribers_are_found_by_server_feature() {
        let (client, server) = addresses();
        let other = feature_address(&device_address("i:99999", "Display").unwrap(), &[1], 1);
        let elsewhere = feature_address(&device_address("i:67890", "HeatPump").unwrap(), &[1], 2);

        let mut relations = Relations::default();
        relations.add_subscription(&client, &server).unwrap();
        relations.add_subscription(&other, &server).unwrap();
        relations.add_subscription(&client, &elsewhere).unwrap();

        let subscribers: Vec<_> = relations.subscribers_of(&server).collect();
        assert_eq!(subscribers.len(), 2);
        assert_eq!(relations.subscribers_of(&elsewhere).count(), 1);
    }

    #[test]
    fn a_disconnect_drops_everything_belonging_to_the_peer() {
        let (client, server) = addresses();
        let mut relations = Relations::default();
        relations.add_binding(&client, &server).unwrap();
        relations.add_subscription(&client, &server).unwrap();

        relations.remove_device(&device_address("i:12345", "ControlBox").unwrap());
        assert!(relations.bindings().is_empty());
        assert!(relations.subscriptions().is_empty());
    }
}
