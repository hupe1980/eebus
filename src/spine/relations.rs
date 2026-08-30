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
use super::address::{is_node_management, same_feature};

/// How many clients may bind to one server feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BindingPolicy {
    /// One binding partner per feature, and the first to ask keeps it.
    ///
    /// The default, because SPINE 1.4.0 makes it the rule and the LPC implementation
    /// guide §3.5 asks implementers to apply it already. Without it, two energy managers
    /// can each hold a binding on the same `LoadControl` feature and fight over the
    /// limit, which SPINE gives no way to resolve.
    #[default]
    SinglePerFeature,
    /// Several clients may bind to the same feature.
    ///
    /// Correct where a feature carries entries belonging to different use cases, each
    /// controlled by a different peer; the server is then responsible for keeping them
    /// apart (LPC implementation guide §3.5).
    MultiplePerFeature,
}

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
            next_binding: 1,
            next_subscription: 1,
            policy,
        }
    }

    /// The binding policy in force.
    pub fn policy(&self) -> BindingPolicy {
        self.policy
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

        if self.policy == BindingPolicy::SinglePerFeature
            && self
                .bindings
                .iter()
                .any(|b| same_feature(&b.server, server))
        {
            return Err(ErrorNumber::CommandRejected);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::{device_address, feature_address, node_management};

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
