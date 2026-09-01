//! A §14a Controllable System on the network: the household end of the exchange.
//!
//! It does what a heat pump, wallbox or battery does — announces itself, waits for a
//! control box to connect, applies the limits it is sent, and falls back to its failsafe
//! value when the control box goes quiet. Run it alongside `steuerbox` on the same
//! subnet, or on two machines.
//!
//! ```sh
//! # First run: prints the SKI and the QR payload, and trusts nobody yet.
//! cargo run --example heat_pump --features runtime,mdns,ring
//!
//! # Trust the control box, whose SKI it printed on its own first run.
//! cargo run --example heat_pump --features runtime,mdns,ring -- \
//!     --trust 5555AAAAFFFF1111CCCC3333EEEEDDDD99992222
//!
//! # The EEBUS reset of SHIP §12.2.2: forget every peer, and the identity with it.
//! cargo run --example heat_pump --features runtime,mdns,ring -- --reset
//! ```
//!
//! What it is here to show, beyond `networked.rs`:
//!
//! * **The Controllable System listens.** A control box connects *to* the household, not
//!   the other way round, so this is the side that binds a port and announces itself.
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
use eebus::ship::{ShipId, ShipTxtRecord};
use eebus::spine::{Engine, LocalDevice, LocalEntity};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent,
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
    simulator::show_identity(&ship_id, ski, port);
    if trust.is_empty() {
        println!(
            "nothing is trusted yet — run the control box, then start this again with\n\
             `--trust <its SKI>`. Until then a caller completes TLS and is held in the\n\
             SHIP pending state, which is what lets a user read its SKI off this screen.\n"
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
    let listener = node.listen(("0.0.0.0", port)).await?;
    let bound = listener.local_addr()?;

    let record = ShipTxtRecord::new(ship_id.clone(), ski)
        .with_brand("eebus-rs")
        .with_model("heat-pump-simulator")
        .with_device_type("HeatPump");
    let _mdns = simulator::announce(&record, ship_id.as_str(), bound.port())?;

    let (engine, mut actor) = build(failsafe, nominal);
    let mut hub = Hub::new(node, engine);
    actor.install(hub.engine_mut(), Duration::ZERO);
    report(&actor);

    // One task accepts; the hub drives everything that is already connected.
    let (incoming, mut accepted) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, from)) => {
                    if incoming.send((stream, from)).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("accept failed: {error}");
                    return;
                }
            }
        }
    });

    loop {
        // Anything the listener has accepted since the last pass. This happens *between*
        // calls to `hub.next()` and never races one: `next` is not cancel-safe, and a
        // `select!` that dropped it part-way through a frame would lose the frame.
        while let Ok((stream, from)) = accepted.try_recv() {
            match hub.accept(stream).await {
                Ok(ski) => println!("[pump] {from} completed the handshake as {ski}"),
                Err(error) => println!("[pump] {from} did not get in: {error}"),
            }
        }
        // A tick a second from now, so the loop comes back to look; the actor's own
        // deadline is folded in with it.
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
            }
            HubEvent::Disconnected { ski, reason } => {
                println!("[pump] {ski} went away: {reason:?}");
            }
            HubEvent::PeerKeysUpdated { .. } => {
                // SHIP §12.1.3 renewed a peer's certificate; the store followed it and
                // has to be written back, or the next restart forgets it.
                let _ = store.save_trust(&trust);
            }
            HubEvent::Connected { .. } => {}
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
    }
}

/// A heat pump with the four features LPC asks of a Controllable System.
fn build(failsafe: f64, nominal: f64) -> (Engine, ControllableSystemActor) {
    let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::electrical_connection_feature(4)),
        )
        .expect("a fresh entity");

    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

    let actor = ControllableSystemActor::new(
        ControllableSystem::new(
            CsConfig::new(failsafe, Duration::from_secs(2 * 3_600)).with_nominal_max(nominal),
            Duration::ZERO,
        ),
        lpc::DIRECTION,
        load_control,
        configuration,
        diagnosis,
    )
    .with_electrical_connection(electrical);
    (engine, actor)
}
