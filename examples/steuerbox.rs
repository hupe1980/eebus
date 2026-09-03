//! A §14a control box on the network: the grid operator's end of the exchange.
//!
//! This is the Steuerbox simulator — the Energy Guard side, the thing a household device
//! has to be tested against and the thing an installation has exactly one of. It finds
//! Controllable Systems over mDNS, connects to the ones it has been told to trust, and
//! writes the limit it is given.
//!
//! ```sh
//! # First run: prints its own SKI and what it finds on the network. A device it finds
//! # but does not trust is offered for pairing; answer `y` to dial it.
//! cargo run --example steuerbox --features runtime,mdns,ring
//!
//! # Or trust the heat pump in advance, from the SKI it printed on its own first run,
//! # and hold the household to 4.2 kW — the §14a EnWG figure.
//! cargo run --example steuerbox --features runtime,mdns,ring -- \
//!     --trust 5555AAAAFFFF1111CCCC3333EEEEDDDD99992222 --limit 4200
//!
//! # A limit that expires on its own, which is what a dimming window looks like.
//! cargo run --example steuerbox --features runtime,mdns,ring -- --limit 4200 --for 900
//!
//! # Release it again, which is a deactivation and not the absence of a message.
//! cargo run --example steuerbox --features runtime,mdns,ring -- --release
//!
//! # Pair with a household device without anybody comparing a SKI: paste the QR payload
//! # the other device printed. This is the SHIP Pairing Service, and this box is `devZ` —
//! # the control unit an installer configures on behalf of the metering point operator.
//! cargo run --example steuerbox --features runtime,mdns,ring,pairing -- \
//!     --pair-with 'SHIP;SKI:…;ID:i:46925_u:HeatPump-1;FPH256:…;SPSEC:…;ENDSHIP;'
//! ```
//!
//! Everything the ordering rules require — the heartbeat before the limit, the two
//! bindings before the write, the opening write §2.11 owes whether or not the grid needs
//! anything — belongs to [`EnergyGuardActor`] and does not appear here.

#[path = "simulator/mod.rs"]
mod simulator;

use core::time::Duration;

use eebus::model::{DeviceType, EntityType};
use eebus::runtime::{Hub, HubEvent, Node, TrustStore, TrustedPeer};
use eebus::ship::pairing::{Nonce, PairingRequest, PairingSecret, Requester, RequesterAction};
use eebus::ship::{ShipId, ShipQr, ShipTxtRecord};
use eebus::spine::{Engine, LocalDevice, LocalEntity};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{self, EnergyGuardActor, GuardEvent, LimitWrite};
use eebus::usecases::lpc;

use simulator::{Args, DeviceStore};

const IANA_PEN: &str = "46925";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::from_env();
    let store = DeviceStore::open(args.value_or("--state", ".eebus-steuerbox"))?;

    if args.flag("--reset") {
        let trust = store.trust();
        println!("EEBUS reset: forgetting {} peer(s)", trust.forget_all());
        store.factory_reset()?;
        return Ok(());
    }

    let product = args.value_or("--name", "ControlBox-1");
    let ship_id = ShipId::new(IANA_PEN, product);
    let identity = store.identity(ship_id.as_str())?;
    let ski = identity.ski;

    let trust: TrustStore = store.trust();
    if let Some(peer) = args.value("--trust") {
        simulator::approve(&trust, &store, TrustedPeer::new(peer.parse()?));
    }

    let port: u16 = args.value_or("--port", "4713").parse()?;
    simulator::show_identity(&ship_id, &identity, port, None);

    // What the grid is asking for. `--release` is not the absence of a limit: LPC
    // implementation guide §2.13 wants a deactivation sent, and sent only when the grid
    // actually permits unlimited operation.
    let required = if args.flag("--release") {
        Some(LimitWrite::deactivated())
    } else {
        args.value("--limit").map(|watts| {
            let watts: f64 = watts.parse().expect("--limit takes watts");
            match args.value("--for") {
                Some(seconds) => LimitWrite::active_for(
                    watts,
                    Duration::from_secs(seconds.parse().expect("--for takes seconds")),
                ),
                None => LimitWrite::active(watts),
            }
        })
    };
    match &required {
        Some(limit) if limit.is_active => println!(
            "the grid requires {:.0} W{}\n",
            limit.watts,
            match limit.duration {
                Some(span) => alloc_for(span),
                None => String::new(),
            }
        ),
        Some(_) => println!("the grid permits unlimited operation\n"),
        None => println!("no limit given; the opening deactivation still goes out (§2.11)\n"),
    }

    let node = Node::new(ship_id.as_str(), ShipTls::new(identity), trust.clone());

    // `devZ`'s end of the Pairing Service: everything needed to ask a household device to
    // trust this one comes off that device's QR code, and the request is signed with the
    // secret printed beside it.
    let mut requester = match args.value("--pair-with") {
        Some(payload) => Some(pairing_request(payload, &node, &trust)?),
        None => None,
    };
    // §4.2: the request goes up before any connection exists — `devA` cannot trust this
    // box until it has heard it, so waiting for a connection would be waiting for
    // something this box's own silence prevents.
    let record = ShipTxtRecord::new(ship_id.clone(), ski)
        .with_brand("eebus-rs")
        .with_model("control-box-simulator")
        .with_device_type("GridConnectionHub");
    let mut mdns = simulator::announce(&record, ship_id.as_str(), port)?;
    if let Some(requester) = requester.as_mut() {
        drive_pairing(&mut mdns, requester, ship_id.as_str(), port)?;
    }

    let (engine, client, diagnosis) = build();
    let mut guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, Duration::ZERO);
    let mut hub = Hub::new(node, engine);
    // The hub browses: a trusted peer it finds is dialled and kept dialled; an untrusted
    // one is reported, and dialled the moment it is approved.
    hub.browse(&mdns)?;
    let answers = simulator::Console::start();

    println!("browsing for _ship._tcp …\n");
    loop {
        hub.wake_at(guard.poll_timeout());
        hub.wake_at(hub.now() + Duration::from_secs(1));

        let now = hub.now();
        let event = match hub.next().await {
            Ok(event) => event,
            Err(error) => {
                eprintln!("the hub stopped: {error}");
                break;
            }
        };

        let mut reports = Vec::new();
        match event {
            HubEvent::Found { peer, trusted } => {
                println!(
                    "found {} at {:?}:{}  {}",
                    peer.ski.to_display_string(),
                    peer.addresses,
                    peer.port,
                    if trusted {
                        "— trusted, dialling"
                    } else {
                        "— not trusted"
                    }
                );
                if !trusted {
                    println!("      pair with it? [y/N] ");
                    answers.ask(peer.ski);
                }
            }
            HubEvent::Lost { instance, .. } => println!("lost {instance}"),
            HubEvent::TrustRequested { ski, origin } => {
                println!(
                    "[box]  {} wants to pair, {origin}\n       trust it? [y/N] ",
                    ski.to_display_string()
                );
                answers.ask(ski);
            }
            HubEvent::Connected { ski, .. } => {
                println!("[box]  connected to {ski}");
                // The clock towards withdrawing the request starts here, and restarts
                // from zero on every reconnection (§4.2 note 1).
                if let Some(requester) = requester.as_mut() {
                    requester.on_connected(now);
                }
            }
            HubEvent::HandshakeFailed { origin, error, .. } => {
                println!("[box]  a connection {origin} failed: {error}");
            }
            HubEvent::PeerDiscovered { device, .. } => {
                let remote = hub.engine().peer(&device).expect("just discovered");
                match limitation::locate(remote, lpc::DIRECTION) {
                    // The guard attaches itself; this is only what to print.
                    Some(_) => {
                        println!("[box]  {} plays the Controllable System", device.as_str());
                        if let Some(limit) = required {
                            guard.require(&device, Some(limit), now);
                        }
                    }
                    None => println!("[box]  {} does not play LPC", device.as_str()),
                }
            }
            HubEvent::Spine(event) => {
                reports.extend(guard.handle_event(hub.engine_mut(), &event, now))
            }
            HubEvent::Tick => {
                reports = guard.handle_timeout(hub.engine_mut(), now);
                // §4.2: fifteen uninterrupted minutes, and the request is withdrawn for
                // good — reboots included, which is why a real device would persist this.
                if let Some(requester) = requester.as_mut() {
                    requester.handle_timeout(now);
                    drive_pairing(&mut mdns, requester, ship_id.as_str(), port)?;
                }
                for (ski, yes) in answers.decided() {
                    if yes {
                        hub.approve(ski);
                        simulator::approve(&trust, &store, TrustedPeer::new(ski));
                    } else {
                        hub.refuse(ski);
                        println!("[box]  refused {}", ski.to_display_string());
                    }
                }
            }
            HubEvent::Disconnected { ski, reason } => {
                println!("[box]  {ski} went away: {reason}");
                if let Some(requester) = requester.as_mut() {
                    requester.on_disconnected();
                }
            }
            HubEvent::PeerKeysUpdated { .. } => {
                let _ = store.save_trust(&trust);
            }
            // This box is `devZ`: it sends pairing requests, it does not receive them.
            HubEvent::Paired { .. } | HubEvent::PairingRefused { .. } => {}
        }

        for report in reports {
            match report {
                GuardEvent::Ready { device } => {
                    println!("[box]  bound to {}", device.as_str());
                }
                GuardEvent::ConstraintsLearned { nominal_max, .. } => println!(
                    "[box]  it can draw at most {:.0} W ({:?})",
                    nominal_max.watts(),
                    nominal_max
                ),
                GuardEvent::LimitAccepted { limit, request, .. } => println!(
                    "[box]  {:.0} W accepted, answering msgCounter {} — the §14a evidence",
                    limit.watts,
                    request.get()
                ),
                GuardEvent::LimitRefused { limit, error, .. } => {
                    println!("[box]  {:.0} W refused: {error}", limit.watts);
                }
                GuardEvent::PeerHeartbeatLost { device } => {
                    println!("[box]  no heartbeat from {}", device.as_str());
                }
            }
        }
    }

    println!("\n§14a evidence, {} record(s):", guard.audit().len());
    for record in guard.audit().records() {
        println!("  {record:?}");
    }
    Ok(())
}

fn alloc_for(span: Duration) -> String {
    format!(" for {} s", span.as_secs())
}

/// Builds the shippairing request this box will announce, from `devA`'s QR payload.
///
/// The payload carries everything §8 needs about the other device: its SHIP ID, the
/// fingerprint of the certificate it will present, and the secret that authenticates the
/// request. Trusting `devA` first is not optional — §4.2 step 1 makes it this box's job
/// to establish the SHIP connection, and a box that announced a request it could not then
/// connect over would be asking for a pairing it cannot use.
fn pairing_request(
    payload: &str,
    node: &Node,
    trust: &TrustStore,
) -> Result<Requester, Box<dyn std::error::Error>> {
    let qr: ShipQr = payload.parse()?;
    let for_id = qr.id.as_ref().ok_or("the QR code carries no SHIP ID")?;
    let for_par = qr
        .certificate_fingerprint
        .ok_or("the QR code carries no FPH256 fingerprint")?;
    let secret = qr
        .pairing_secret
        .as_deref()
        .ok_or("the QR code carries no SPSEC pairing secret")?;
    let secret = PairingSecret::from_hex(secret)?;

    trust.remember(TrustedPeer::new(qr.ski).with_ship_id(for_id.as_str().to_string()));

    // §5.4 and §6.3: a fresh nonce per request, from a cryptographic generator.
    let mut bytes = [0u8; Nonce::LEN];
    eebus::tls::random(&mut bytes)?;

    let request = PairingRequest::new(
        for_id.as_str(),
        for_par,
        node.ship_id(),
        node.fingerprint(),
        Nonce::from_bytes(bytes),
    );
    println!(
        "will ask {} to trust this box by shippairing request\n",
        for_id.as_str()
    );
    Ok(Requester::new(request.sign(&secret)?))
}

/// Carries out whatever the requester has decided: put the request on the air, or take it
/// off for good.
///
/// §5.3: the port in the SRV record is required by DNS-SD and must be one nothing listens
/// on, because this version of the specification defines no protocol over it — so the
/// SHIP port is deliberately not reused here. The instance name follows §5.2's
/// recommendation, the SHIP instance with a counter appended.
fn drive_pairing(
    mdns: &mut eebus::mdns::Mdns,
    requester: &mut Requester,
    instance: &str,
    ship_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    match requester.poll_action() {
        Some(RequesterAction::Announce) => {
            mdns.announce_pairing(
                &format!("{instance}#1"),
                requester.announcement(),
                ship_port.wrapping_add(1),
                &[std::net::IpAddr::from([127, 0, 0, 1])],
            )?;
            println!("[box]  announcing a shippairing request on _shippairing._tcp");
        }
        Some(RequesterAction::Withdraw) => {
            mdns.withdraw_pairing()?;
            println!("[box]  the pairing has held for 15 minutes; request withdrawn");
        }
        None => {}
    }
    Ok(())
}

/// The control box: an Energy Guard on a `GridGuard` entity.
fn build() -> (
    Engine,
    eebus::model::FeatureAddress,
    eebus::model::FeatureAddress,
) {
    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .expect("a valid device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard)
                .with_feature(limitation::client_feature(1))
                .with_feature(limitation::device_diagnosis_feature(2)),
        )
        .expect("a fresh entity");

    let client = device.address_of(&[1], 1);
    let diagnosis = device.address_of(&[1], 2);
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
    (engine, client, diagnosis)
}
