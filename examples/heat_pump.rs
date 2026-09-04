//! A §14a Controllable System on the network: the household end of the exchange.
//!
//! It does what a heat pump, wallbox or battery does — announces itself, waits for a
//! control box to connect, applies the limits it is sent, and falls back to its failsafe
//! value when the control box goes quiet. Run it alongside `steuerbox` on the same
//! subnet, or on two machines.
//!
//! ```sh
//! # First run: prints the SKI and the QR payload, and trusts nobody yet. A control box
//! # that connects is held in the SHIP pending state and its SKI is printed; answer
//! # `y` on the terminal to pair, exactly as a device with a button would.
//! cargo run --example heat_pump --features runtime,mdns,ring
//!
//! # Or trust the control box in advance, from the SKI it printed on its own first run.
//! cargo run --example heat_pump --features runtime,mdns,ring -- \
//!     --trust 5555AAAAFFFF1111CCCC3333EEEEDDDD99992222
//!
//! # The EEBUS reset of SHIP §12.2.2: forget every peer, and the identity with it.
//! cargo run --example heat_pump --features runtime,mdns,ring -- --reset
//!
//! # Or pair without anybody comparing a SKI: the SHIP Pairing Service. This is `devA`,
//! # and the secret it prints in its QR code is what a control unit has to be configured
//! # with. See `steuerbox --pair-with`.
//! cargo run --example heat_pump --features runtime,mdns,ring,pairing -- --pairing
//! ```
//!
//! What it is here to show, beyond `networked.rs`:
//!
//! * **The Controllable System listens.** A control box connects *to* the household, not
//!   the other way round, so this is the side that binds a port and announces itself.
//! * **Pairing is interactive.** An unknown peer is reported as `TrustRequested` while
//!   it waits in the SHIP hello phase, and `hub.approve` completes the handshake it is
//!   waiting in — no reconnection, no SKI typed in advance.
//! * **…or nobody is asked at all.** With `--pairing`, a control unit that proves it
//!   knows this device's printed secret is trusted on the strength of its certificate
//!   fingerprint, which is the whole point of the Pairing Service.
//! * **Identity and trust survive a restart.** A device that came back with a new SKI
//!   would have to be paired again by hand every time, which is why SHIP §12.1.1 asks for
//!   the opposite.
//! * **The tester hooks are printed on every change** — the `lpc:` signals a certification
//!   laboratory reads, which is the only way to watch a timed transition go by.

#[path = "simulator/mod.rs"]
mod simulator;

use core::time::Duration;

use eebus::model::{DeviceType, EntityType};
use eebus::runtime::{Hub, HubEvent, Node, TrustStore, TrustedPeer};
use eebus::ship::pairing::Receiver;
use eebus::ship::{ShipId, ShipTxtRecord};
use eebus::spine::{Engine, LocalDevice, LocalEntity};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, ControllableSystemBuilder, CsConfig,
    CsEvent, CsFeatures,
};
use eebus::usecases::lpc;
use eebus::usecases::signals::Signals;

use simulator::{Args, DeviceStore};

const IANA_PEN: &str = "46925";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::from_env();
    let store = DeviceStore::open(args.value_or("--state", ".eebus-heatpump"))?;

    if args.flag("--reset") {
        // The half this crate owns, and the half the device owns.
        let trust = store.trust();
        println!("EEBUS reset: forgetting {} peer(s)", trust.forget_all());
        store.factory_reset()?;
        return Ok(());
    }

    let product = args.value_or("--name", "HeatPump-1");
    let ship_id = ShipId::new(IANA_PEN, product);
    let identity = store.identity(ship_id.as_str())?;
    let ski = identity.ski;

    let trust: TrustStore = store.trust();
    if let Some(peer) = args.value("--trust") {
        simulator::approve(&trust, &store, TrustedPeer::new(peer.parse()?));
    }

    let port: u16 = args.value_or("--port", "4712").parse()?;
    // `devA`'s secret is only worth printing where the pairing service is on: it is key
    // material, and a QR code that carries one when nothing will honour it is a leak for
    // no purpose.
    let secret = args.flag("--pairing").then(|| store.pairing_secret());
    let secret = secret.transpose()?;
    simulator::show_identity(&ship_id, &identity, port, secret.as_ref());
    if trust.is_empty() {
        println!(
            "nothing is trusted yet — a control box that connects is held in the SHIP\n\
             pending state and asked about here; answer `y` to pair it, or start again\n\
             with `--trust <its SKI>`.\n"
        );
    } else {
        for peer in trust.peers() {
            println!("trusting {}", peer.ski.to_display_string());
        }
        println!();
    }

    let failsafe: f64 = args.value_or("--failsafe", "4200").parse()?;
    let nominal: f64 = args.value_or("--nominal-max", "11000").parse()?;

    let node = Node::new(ship_id.as_str(), ShipTls::new(identity), trust.clone());
    let fingerprint = node.fingerprint();
    let (engine, actor) = build(failsafe, nominal);
    let mut hub = Hub::new(node, engine);

    // The hub owns the listener: every socket it accepts goes through TLS and the SHIP
    // handshake in the background, and arrives here as an event.
    let bound = hub.listen(("0.0.0.0", port)).await?;
    let record = ShipTxtRecord::new(ship_id.clone(), ski)
        .with_brand("eebus-rs")
        .with_model("heat-pump-simulator")
        .with_device_type("HeatPump");
    let mdns = simulator::announce(&record, ship_id.as_str(), bound.port())?;

    // `devA`'s end of the Pairing Service: browse for requests naming this device, and
    // trust the one that proves it knows the secret. Everything the specification asks
    // for is in the two calls — the replay guard of §11 comes off disk, and §4.3's rule
    // about when a working pairing may be replaced is the hub's.
    if let Some(secret) = secret {
        hub.accept_pairing_requests(
            Receiver::new(ship_id.as_str(), fingerprint, secret).with_guard(store.replay_guard()),
        );
        hub.browse_pairing(&mdns)?;
        println!(
            "accepting shippairing requests addressed to {}\n",
            ship_id.as_str()
        );
    }

    let now = hub.now();
    let mut actor = actor.install(hub.engine_mut(), now);
    report(&actor);

    // A person at a terminal, standing in for the button on a real device.
    let answers = simulator::Console::start();

    loop {
        // The actor's own deadline is what the hub waits on; a question to the user is
        // looked at once a second, which is what the tick is for.
        hub.wake_at(actor.poll_timeout());
        hub.wake_at(hub.now() + Duration::from_secs(1));

        let now = hub.now();
        let event = match hub.next().await {
            Ok(event) => event,
            Err(error) => {
                eprintln!("the hub stopped: {error}");
                break;
            }
        };
        match event {
            HubEvent::TrustRequested { peer, origin } => {
                println!(
                    "[pump] {} wants to pair, {origin}\n       trust it? [y/N] ",
                    peer.ski.to_display_string()
                );
                answers.ask(peer.ski);
            }
            HubEvent::Connected { ski, version } => {
                println!("[pump] {ski} completed the handshake (SHIP {version:?})");
                // A paired control unit's SKI is learned here rather than from its
                // request, which does not carry one, so the store has just gained
                // something worth keeping.
                let _ = store.save_trust(&trust);
            }
            HubEvent::HandshakeFailed { origin, error, .. } => {
                println!("[pump] a connection {origin} did not get in: {error}");
            }
            HubEvent::PeerDiscovered { device, ski } => {
                println!("[pump] {ski} is {}", device.as_str());
            }
            HubEvent::Spine(event) => {
                if let Some(decision) = actor.handle_event(hub.engine_mut(), &event, now) {
                    announce_decision(&decision);
                    report(&actor);
                }
            }
            HubEvent::Tick => {
                if let Some(decision) = actor.handle_timeout(hub.engine_mut(), now) {
                    announce_decision(&decision);
                    report(&actor);
                }
                for (ski, yes) in answers.decided() {
                    if yes {
                        hub.approve(ski);
                        simulator::approve(&trust, &store, TrustedPeer::new(ski));
                    } else {
                        hub.refuse(ski);
                        println!("[pump] refused {}", ski.to_display_string());
                    }
                }
            }
            HubEvent::Disconnected { ski, reason } => {
                println!("[pump] {ski} went away: {reason}");
            }
            HubEvent::Paired { unit, displaced } => {
                println!(
                    "[pump] {} paired itself with a valid shippairing request",
                    unit.ship_id
                );
                if let Some(displaced) = displaced {
                    println!(
                        "[pump] and replaced {}, which is now untrusted",
                        displaced.ship_id
                    );
                }
                // Both halves are persisted, and both matter: the trust, or the pairing
                // is gone at the next restart; the replay guard, or the announcement that
                // established it can be replayed off a capture.
                let _ = store.save_trust(&trust);
                if let Some(guard) = hub.pairing_guard() {
                    let _ = store.save_replay_guard(guard);
                }
            }
            HubEvent::PairingRefused { instance, error } => {
                println!("[pump] a shippairing request ({instance}) was refused: {error}");
            }
            HubEvent::PeerKeysUpdated { .. } => {
                // SHIP §12.1.3 renewed a peer's certificate; the store followed it and
                // has to be written back, or the next restart forgets it.
                let _ = store.save_trust(&trust);
            }
            _ => {}
        }
    }
    Ok(())
}

/// What a certification laboratory reads off the debug interface.
fn report(actor: &ControllableSystemActor) {
    println!("{}", actor.signals(()));
    println!();
}

fn announce_decision(decision: &CsEvent) {
    match decision {
        CsEvent::LimitDecided { write, outcome, .. } => println!(
            "[pump] a {:.0} W limit was {}",
            write.watts,
            if outcome.is_accepted() {
                "accepted"
            } else {
                "refused"
            }
        ),
        CsEvent::LimitUnreadable { .. } => println!("[pump] a write on the limit was unreadable"),
        CsEvent::FailsafeDecided { outcome, .. } => {
            println!("[pump] a failsafe write was {outcome:?}")
        }
        CsEvent::GuardIdentified { .. } => println!("[pump] the Energy Guard has both bindings"),
        CsEvent::StateChanged { from, to } => println!("[pump] {from} → {to}"),
        // The crate keeps adding events; a consumer that does not need them says so.
        _ => {}
    }
}

/// A heat pump with the four features LPC asks of a Controllable System.
fn build(failsafe: f64, nominal: f64) -> (Engine, ControllableSystemBuilder) {
    let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::device_diagnosis_client_feature(5))
                .with_feature(limitation::electrical_connection_feature(4)),
        )
        .expect("a fresh entity");

    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);
    let diagnosis_client = device.address_of(&[1], 5);
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

    let actor = ControllableSystemActor::builder(
        ControllableSystem::new(
            CsConfig::new(failsafe, Duration::from_secs(2 * 3_600)).with_nominal_max(nominal),
            Duration::ZERO,
        ),
        lpc::DIRECTION,
        CsFeatures {
            load_control,
            device_configuration: configuration,
            device_diagnosis: diagnosis,
            device_diagnosis_client: diagnosis_client,
        },
    )
    .with_electrical_connection(electrical);
    (engine, actor)
}
