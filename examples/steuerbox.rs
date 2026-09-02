//! A §14a control box on the network: the grid operator's end of the exchange.
//!
//! This is the Steuerbox simulator — the Energy Guard side, the thing a household device
//! has to be tested against and the thing an installation has exactly one of. It finds
//! Controllable Systems over mDNS, connects to the ones it has been told to trust, and
//! writes the limit it is given.
//!
//! ```sh
//! # First run: prints its own SKI, finds nothing, trusts nobody.
//! cargo run --example steuerbox --features runtime,mdns,ring
//!
//! # Trust the heat pump, whose SKI it printed on its own first run, and hold the
//! # household to 4.2 kW — the §14a EnWG figure.
//! cargo run --example steuerbox --features runtime,mdns,ring -- \
//!     --trust 5555AAAAFFFF1111CCCC3333EEEEDDDD99992222 --limit 4200
//!
//! # A limit that expires on its own, which is what a dimming window looks like.
//! cargo run --example steuerbox --features runtime,mdns,ring -- --limit 4200 --for 900
//!
//! # Release it again, which is a deactivation and not the absence of a message.
//! cargo run --example steuerbox --features runtime,mdns,ring -- --release
//! ```
//!
//! Everything the ordering rules require — the heartbeat before the limit, the two
//! bindings before the write, the opening write §2.11 owes whether or not the grid needs
//! anything — belongs to [`EnergyGuardActor`] and does not appear here.

#[path = "simulator/mod.rs"]
mod simulator;

use core::time::Duration;

use eebus::mdns::BrowseEvent;
use eebus::model::{DeviceType, EntityType};
use eebus::runtime::{Hub, HubEvent, Node, TrustStore, TrustedPeer};
use eebus::ship::{ShipId, ShipTxtRecord};
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
    simulator::show_identity(&ship_id, ski, port);

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
    let record = ShipTxtRecord::new(ship_id.clone(), ski)
        .with_brand("eebus-rs")
        .with_model("control-box-simulator")
        .with_device_type("GridConnectionHub");
    let mdns = simulator::announce(&record, ship_id.as_str(), port)?;
    let browse = mdns.browse()?;

    let (engine, client, diagnosis) = build();
    let mut guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, Duration::ZERO);
    let mut hub = Hub::new(node, engine);

    println!("browsing for _ship._tcp …\n");
    loop {
        // Anything mDNS has found since the last pass, without blocking the event loop.
        while let Some(event) = browse.try_recv() {
            match event {
                BrowseEvent::Found(found) => {
                    let known = trust.is_trusted(&found.ski);
                    println!(
                        "found {} at {:?}:{}  {}",
                        found.ski.to_display_string(),
                        found.addresses,
                        found.port,
                        if known {
                            "— trusted"
                        } else {
                            "— not trusted"
                        }
                    );
                    if !known {
                        println!("      start again with `--trust {}`", found.ski);
                        continue;
                    }
                    if let Some(address) = found.socket_address()
                        && hub.peers().all(|peer| peer != found.ski)
                    {
                        match hub.connect(address).await {
                            Ok(ski) => println!("      connected to {ski}"),
                            Err(error) => println!("      could not connect: {error}"),
                        }
                    }
                }
                BrowseEvent::Lost { instance } => println!("lost {instance}"),
            }
        }

        // A tick a second from now, so the loop comes back to look at mDNS. `hub.next()`
        // is not cancel-safe, so it is never wrapped in a `timeout` or a `select!`: a
        // dropped read loses a frame, and a lost frame here is a subscription that is
        // never granted and a limit refused for want of the heartbeat behind it.
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
            HubEvent::Tick => reports = guard.handle_timeout(hub.engine_mut(), now),
            HubEvent::Disconnected { ski, reason } => {
                println!("[box]  {ski} went away: {reason:?}");
            }
            HubEvent::PeerKeysUpdated { .. } => {
                let _ = store.save_trust(&trust);
            }
            HubEvent::Connected { .. } => {}
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
