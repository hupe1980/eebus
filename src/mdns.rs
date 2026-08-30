//! Announcing a SHIP node on the local network, and finding the ones that announce.
//!
//! SHIP nodes find each other with DNS-SD over multicast DNS: a node registers
//! `_ship._tcp` with a TXT record naming its SKI, its SHIP ID and what kind of device it
//! is, and anything looking for a peer browses for the same service type. There is no
//! central registry and no cloud — a heat pump and a control box on the same subnet
//! discover each other and nothing else needs to be running.
//!
//! [`ShipTxtRecord`] already knows the record's contents and the rules for reading it;
//! this module is the part that puts it on the wire and takes it off again. What it adds
//! on top is refusal: a service whose TXT record is malformed, or whose SKI cannot be
//! read, is dropped rather than reported with the missing pieces guessed at, because a
//! SKI is the one thing a trust decision rests on.
//!
//! Requires the `mdns` feature.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::IpAddr;

use crate::ship::{DiscoveryError, PAIRING_SERVICE_TYPE, SERVICE_TYPE, ShipTxtRecord, Ski};

/// The domain mDNS-SD service types live in.
const LOCAL: &str = ".local.";

/// Why announcing or browsing failed.
#[derive(Debug, thiserror::Error)]
pub enum MdnsError {
    /// The mDNS daemon failed.
    #[error("mDNS failed: {0}")]
    Daemon(#[from] mdns_sd::Error),
    /// The TXT record could not be built.
    #[error("the TXT record is invalid: {0}")]
    Record(#[from] DiscoveryError),
}

/// What a [`Browse`] reported.
///
/// A node that leaves is news too: an application that dials what it discovers would
/// otherwise keep a departed peer in its redial schedule for the life of the process.
// Boxing the arrival would make every caller dereference to reach the fields it wants,
// for a stack frame nobody is short of: these arrive one at a time off a channel.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseEvent {
    /// A SHIP node announced itself, or changed what it announces.
    Found(Discovered),
    /// A SHIP node withdrew its announcement.
    ///
    /// Only the instance name comes with it: the TXT record that carried the SKI is what
    /// has been withdrawn. [`Hub::forget_discovered`](crate::runtime::Hub::forget_discovered)
    /// takes it from here.
    Lost {
        /// The instance name that has gone.
        instance: String,
    },
}

/// A SHIP node found on the network.
///
/// Everything here came off the wire, so treat it as a claim rather than a fact — the
/// only part that is verified is the SKI, and only once TLS has proved the peer holds the
/// matching key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// The node's SKI, as it advertised it.
    ///
    /// It is checked against the peer's certificate when a connection is made; until
    /// then it is only what the node says about itself.
    pub ski: Ski,
    /// The rest of the TXT record: SHIP ID, brand, model, device categories.
    pub record: ShipTxtRecord,
    /// The addresses it can be reached at.
    pub addresses: Vec<IpAddr>,
    /// The port SHIP is listening on.
    pub port: u16,
    /// The instance name, which is what identifies the service to mDNS.
    pub instance: String,
}

impl Discovered {
    /// The first address and port, in the form [`crate::runtime::Node::connect`] takes.
    pub fn socket_address(&self) -> Option<core::net::SocketAddr> {
        self.addresses
            .first()
            .map(|address| core::net::SocketAddr::new(*address, self.port))
    }
}

/// Announces this node, and browses for others.
///
/// One daemon serves both directions; drop it and the announcement is withdrawn.
pub struct Mdns {
    daemon: mdns_sd::ServiceDaemon,
    announced: Vec<String>,
}

impl core::fmt::Debug for Mdns {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mdns")
            .field("announced", &self.announced)
            .finish_non_exhaustive()
    }
}

impl Mdns {
    /// Starts the mDNS responder.
    pub fn new() -> Result<Self, MdnsError> {
        Ok(Self {
            daemon: mdns_sd::ServiceDaemon::new()?,
            announced: Vec::new(),
        })
    }

    /// Announces this node as `_ship._tcp`.
    ///
    /// `instance` is the service instance name, which SHIP leaves to the implementation;
    /// the SHIP ID makes a good one, being unique and meaningful to a person reading a
    /// packet capture.
    pub fn announce(
        &mut self,
        instance: &str,
        record: &ShipTxtRecord,
        port: u16,
        addresses: &[IpAddr],
    ) -> Result<(), MdnsError> {
        self.register(SERVICE_TYPE, instance, record, port, addresses)
    }

    /// Announces this node as `_shippairing._tcp`, for the Pairing Service.
    pub fn announce_pairing(
        &mut self,
        instance: &str,
        record: &ShipTxtRecord,
        port: u16,
        addresses: &[IpAddr],
    ) -> Result<(), MdnsError> {
        self.register(PAIRING_SERVICE_TYPE, instance, record, port, addresses)
    }

    fn register(
        &mut self,
        service_type: &str,
        instance: &str,
        record: &ShipTxtRecord,
        port: u16,
        addresses: &[IpAddr],
    ) -> Result<(), MdnsError> {
        let properties: Vec<(String, String)> = record.to_pairs()?;
        let host = alloc::format!("{}.local.", sanitise(instance));
        let service = mdns_sd::ServiceInfo::new(
            &alloc::format!("{service_type}{LOCAL}"),
            instance,
            &host,
            addresses,
            port,
            &properties[..],
        )?;
        let full = service.get_fullname().to_string();
        self.daemon.register(service)?;
        self.announced.push(full);
        Ok(())
    }

    /// Withdraws every announcement this node made.
    ///
    /// SHIP asks a node to stop announcing while it cannot accept another connection, so
    /// this is called when the connection table fills as well as at shutdown.
    pub fn withdraw(&mut self) -> Result<(), MdnsError> {
        for fullname in self.announced.drain(..) {
            self.daemon.unregister(&fullname)?;
        }
        Ok(())
    }

    /// Browses for SHIP nodes, reporting each as it resolves.
    ///
    /// The receiver stays open until it is dropped; a node that leaves and comes back
    /// arrives again, so the caller decides how long a sighting stays interesting.
    pub fn browse(&self) -> Result<Browse, MdnsError> {
        Ok(Browse {
            events: self
                .daemon
                .browse(&alloc::format!("{SERVICE_TYPE}{LOCAL}"))?,
        })
    }

    /// Browses for nodes offering the Pairing Service.
    pub fn browse_pairing(&self) -> Result<Browse, MdnsError> {
        Ok(Browse {
            events: self
                .daemon
                .browse(&alloc::format!("{PAIRING_SERVICE_TYPE}{LOCAL}"))?,
        })
    }
}

impl Drop for Mdns {
    fn drop(&mut self) {
        // Withdrawing on the way out is what stops a peer trying to reach a node that has
        // gone; failing to withdraw is not worth a panic in a destructor.
        let _ = self.withdraw();
    }
}

/// An open browse, delivering nodes as they are found.
pub struct Browse {
    events: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
}

impl core::fmt::Debug for Browse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Browse")
    }
}

impl Browse {
    /// Waits for the next arrival or departure.
    ///
    /// Services whose TXT record cannot be read are skipped rather than reported: a
    /// record without a readable SKI names nothing a trust decision could be made about.
    /// An application that only cares about arrivals matches [`BrowseEvent::Found`].
    pub fn recv(&self) -> Option<BrowseEvent> {
        loop {
            if let Some(event) = interpret(self.events.recv().ok()?) {
                return event;
            }
        }
    }

    /// The next arrival or departure, or [`None`] if nothing has happened yet.
    pub fn try_recv(&self) -> Option<BrowseEvent> {
        loop {
            if let Some(event) = interpret(self.events.try_recv().ok()?) {
                return event;
            }
        }
    }

    /// The next arrival or departure, giving up after `timeout`.
    pub fn recv_timeout(&self, timeout: core::time::Duration) -> Option<BrowseEvent> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            if let Some(event) = interpret(self.events.recv_timeout(left).ok()?) {
                return event;
            }
        }
    }
}

/// Turns one mDNS event into a [`BrowseEvent`].
///
/// `None` means "nothing to report, keep waiting"; `Some(None)` means the search itself
/// has ended and the caller should stop.
fn interpret(event: mdns_sd::ServiceEvent) -> Option<Option<BrowseEvent>> {
    match event {
        mdns_sd::ServiceEvent::ServiceResolved(service) => {
            read(&service).map(|found| Some(BrowseEvent::Found(found)))
        }
        mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => Some(Some(BrowseEvent::Lost {
            instance: instance_of(&fullname),
        })),
        mdns_sd::ServiceEvent::SearchStopped(_) => Some(None),
        _ => None,
    }
}

/// The instance part of a DNS-SD full name, which is what identifies one service.
fn instance_of(fullname: &str) -> String {
    fullname
        .split_once('.')
        .map(|(instance, _)| instance.to_string())
        .unwrap_or_else(|| fullname.to_string())
}

/// Turns a resolved service into a [`Discovered`], or nothing if it is not usable.
fn read(service: &mdns_sd::ResolvedService) -> Option<Discovered> {
    let pairs: Vec<(String, String)> = service
        .txt_properties
        .iter()
        .map(|property| (property.key().to_string(), property.val_str().to_string()))
        .collect();
    let record = ShipTxtRecord::from_pairs(&pairs).ok()?;

    Some(Discovered {
        ski: record.ski,
        record,
        addresses: service.addresses.iter().map(|a| a.to_ip_addr()).collect(),
        port: service.port,
        instance: instance_of(&service.fullname),
    })
}

/// Makes an instance name usable as a host name.
///
/// A SHIP ID contains colons and underscores, and a host label may contain neither.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ship::{DeviceCategory, ShipId};

    fn a_record() -> ShipTxtRecord {
        ShipTxtRecord::new(
            ShipId::new("46925", "HeatPump-1"),
            "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap(),
        )
        .with_brand("ExampleBrand")
        .with_model("HP-3000")
        .with_categories([DeviceCategory::Hvac])
    }

    #[test]
    fn a_ship_id_becomes_a_usable_host_label() {
        assert_eq!(sanitise("i:46925_u:HeatPump-1"), "i-46925-u-HeatPump-1");
    }

    #[test]
    fn the_record_a_node_announces_is_the_one_a_peer_reads_back() {
        // The wire form is the pair list; this is the round trip that browsing performs,
        // without needing a network.
        let record = a_record();
        let pairs = record.to_pairs().unwrap();
        let read_back = ShipTxtRecord::from_pairs(&pairs).unwrap();
        assert_eq!(read_back.ski, record.ski);
        assert_eq!(read_back.id, record.id);
        assert_eq!(read_back.brand, record.brand);
    }
}
