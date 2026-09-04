//! This crate against `eebus-go`, over a real socket.
//!
//! The device corpus in `tests/real_devices.rs` proves this crate can *read* what other
//! implementations write. It cannot prove anything about a conversation: the SHIP
//! handshake against a peer we did not write, the double-connection rule this crate and
//! `ship-go` deliberately resolve differently, binding and subscription negotiation, or a
//! limit written by something that did not agree in advance what a limit is.
//!
//! This does. A container runs `eebus-go`'s own `examples/controlbox` — a complete Energy
//! Guard, unmodified, at a pinned revision — and this crate stands up the Controllable
//! System on the other end. What is asserted is the §14a exchange: TLS 1.2 with mutual
//! authentication, the five-phase SHIP handshake, SPINE discovery, the binding and the
//! subscription, and a consumption limit that `eebus-go` writes and this crate accepts.
//!
//! # Why not `testcontainers`
//!
//! It is the obvious tool and it was the wrong one here. This crate's dependency set is
//! small and auditable on purpose — that is most of what it offers a device manufacturer
//! facing the Cyber Resilience Act — and `testcontainers` brings a Docker API client and
//! its transitive HTTP stack into `cargo test` for every contributor, including the ones
//! who will never run this. Driving the `docker` CLI is forty lines of `Command`, adds
//! **no dependency at all**, and does not couple the test to an async runtime it does not
//! otherwise need.
//!
//! # Running it
//!
//! ```sh
//! cargo test --features interop,full --test interop -- --nocapture
//! ```
//!
//! It is behind `required-features` so it never runs by accident: an ordinary `cargo test`
//! stays hermetic, needs no network and finishes in seconds, and a contributor without
//! Docker is not blocked. The image is built from a **pinned** `eebus-go` revision, so a
//! failure here means "we broke" rather than "they changed" — which is the only thing that
//! makes an interop test worth reading.

#![cfg(all(feature = "interop", feature = "runtime"))]

use std::process::Command;
use std::time::{Duration, Instant};

use eebus::cert::{self, CertParams};
use eebus::model::{DeviceType, EntityType};
use eebus::runtime::{Hub, HubEvent, Node, TrustStore};
use eebus::ship::Ski;
use eebus::spine::{Engine, LocalDevice, LocalEntity};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, ControllableSystemBuilder, CsConfig,
    CsEvent, CsFeatures, EnergyGuardActor, GuardEvent, LimitWrite, LimitationState,
};
use eebus::usecases::lpc;

const IMAGE: &str = "eebus-interop-controlbox:pinned";
const FAILSAFE_WATTS: f64 = 4_200.0;
const NOMINAL_MAX_WATTS: f64 = 11_000.0;

/// A container, stopped when the test ends however it ends.
struct Peer {
    id: String,
    port: u16,
    ski: Ski,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.id]).output();
    }
}

fn docker(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| format!("docker {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "docker {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Builds the pinned image and starts one side of `eebus-go`, trusting `ours`.
///
/// `role` is the binary the image carries: `controlbox` for their Energy Guard, `evse`
/// for their Controllable System.
fn start_peer(role: &str, ours: &Ski) -> Result<Peer, String> {
    docker(&["build", "-q", "-t", IMAGE, "tests/interop"])?;

    let ski = ours.to_string();
    let id = docker(&[
        // Deliberately not `--rm`: if the peer exits, its log is the only evidence of
        // why, and `--rm` deletes it at exactly the moment it becomes interesting. `Drop`
        // removes the container instead.
        "run",
        "-d",
        "-p",
        "127.0.0.1:0:4712",
        IMAGE,
        role,
        "-port",
        "4712",
        "-remoteski",
        &ski,
    ])?
    .trim()
    .to_string();
    let peer_id = id.clone();

    // The port Docker chose, and the SKI the control box generated for itself. Both are
    // only knowable after it has started, which is why neither is hard-coded.
    let mapped = docker(&["port", &id, "4712/tcp"])?;
    let port: u16 = mapped
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|p| p.trim().parse().ok())
        .ok_or_else(|| format!("cannot read the mapped port from {mapped:?}"))?;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let logs = docker(&["logs", &peer_id]).unwrap_or_default();
        if let Some(hex) = logs
            .lines()
            .find_map(|line| line.split("Local SKI:").nth(1))
            .map(str::trim)
        {
            // The control box's own certificate is generated at start-up, so its SKI is
            // different every run. Reading it back is also this crate's SKI derivation
            // agreeing with `ship-go`'s about the same key.
            let ski: Ski = hex
                .parse()
                .map_err(|e| format!("`{hex}` is not a SKI this crate can read: {e:?}"))?;
            return Ok(Peer {
                id: peer_id,
                port,
                ski,
            });
        }
        if Instant::now() > deadline {
            let _ = Command::new("docker").args(["rm", "-f", &peer_id]).output();
            return Err(format!("the control box never announced a SKI:\n{logs}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Starts one side of `eebus-go` on the **host's** network, so it can reach a listener
/// outside the container.
///
/// The other direction from [`start_peer`], and the one a household device is actually in:
/// §14a has the control box dial and the household appliance listen, so the peer has to be
/// able to reach a `hemsd`-shaped process on the host rather than the other way round.
///
/// `eebus-go`'s examples find their peer over **mDNS** — `-remoteski` says which SKI to
/// trust, not where it is — and multicast does not cross Docker's bridge network. So this
/// needs `--network host`, which is Linux only in practice: on Docker Desktop the
/// container runs inside a VM whose network is not the host's, `host.docker.internal`
/// resolves to the VM's gateway, and no amount of it carries multicast. Where that is not
/// available the test skips rather than failing, and says which of the two it was.
fn start_peer_on_host_network(role: &str, ours: &Ski) -> Result<Peer, String> {
    docker(&["build", "-q", "-t", IMAGE, "tests/interop"])?;

    let ski = ours.to_string();
    let id = docker(&[
        "run",
        "-d",
        // The whole point: the peer sees the host's interfaces, so the host's mDNS
        // announcement reaches it and the port it dials is the host's.
        "--network",
        "host",
        IMAGE,
        role,
        "-port",
        "4713",
        "-remoteski",
        &ski,
    ])?
    .trim()
    .to_string();
    let peer_id = id.clone();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let logs = docker(&["logs", &peer_id]).unwrap_or_default();
        if let Some(hex) = logs
            .lines()
            .find_map(|line| line.split("Local SKI:").nth(1))
            .map(str::trim)
        {
            let ski: Ski = hex
                .parse()
                .map_err(|e| format!("`{hex}` is not a SKI this crate can read: {e:?}"))?;
            return Ok(Peer {
                id: peer_id,
                // Nothing dials it: it is the one dialling.
                port: 0,
                ski,
            });
        }
        if Instant::now() > deadline {
            let _ = Command::new("docker").args(["rm", "-f", &peer_id]).output();
            return Err(format!("the peer never announced a SKI:\n{logs}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether `--network host` actually shares the host's network on this machine.
///
/// Docker accepts the flag everywhere and honours it only on a native Linux daemon;
/// elsewhere the container lands in the VM's network and the mDNS this test turns on
/// never leaves it. Asking the daemon what it is is the only reliable answer.
fn has_host_networking() -> bool {
    let out = Command::new("docker")
        .args(["info", "--format", "{{.OSType}}/{{.OperatingSystem}}"])
        .output();
    let Ok(out) = out else {
        return false;
    };
    let info = String::from_utf8_lossy(&out.stdout).to_lowercase();
    info.starts_with("linux/") && !info.contains("docker desktop")
}

/// A heat pump with the four features LPC asks of a Controllable System.
fn heat_pump() -> (Engine, ControllableSystemBuilder) {
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

    let features = CsFeatures {
        load_control: device.address_of(&[1], 1),
        device_configuration: device.address_of(&[1], 2),
        device_diagnosis: device.address_of(&[1], 3),
        device_diagnosis_client: device.address_of(&[1], 5),
    };
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

    let actor = ControllableSystemActor::builder(
        ControllableSystem::new(
            CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                .with_nominal_max(NOMINAL_MAX_WATTS),
            Duration::ZERO,
        ),
        lpc::DIRECTION,
        features,
    )
    .with_electrical_connection(electrical);
    (engine, actor)
}

/// `eebus-go` writes a §14a limit and this crate applies it.
///
/// The whole path, against an implementation that shares no code with this one: TLS 1.2
/// with mutual authentication, the SHIP handshake, SPINE discovery, the binding and
/// subscription `eebus-go` asks for, its heartbeat, and finally a
/// `loadControlLimitListData` write that moves this crate's state machine to `limited`
/// and is acknowledged.
#[tokio::test(flavor = "multi_thread")]
async fn eebus_go_writes_a_limit_and_this_crate_applies_it() {
    if Command::new("docker").arg("info").output().is_err() {
        eprintln!("skipping: docker is not available");
        return;
    }

    let identity = cert::self_signed(CertParams::new("i:46925_HeatPump-1")).expect("a certificate");
    let trust = TrustStore::new();
    let node = Node::new("i:46925_HeatPump-1", ShipTls::new(identity), trust.clone());
    let ours = node.ski();

    let peer = start_peer("controlbox", &ours).expect("the control box started");
    println!("control box SKI {} on port {}", peer.ski, peer.port);
    trust.trust(peer.ski);

    let (engine, actor) = heat_pump();
    let mut hub = Hub::new(node, engine);
    let mut actor = actor.install(hub.engine_mut(), Duration::ZERO);
    hub.dial(std::net::SocketAddr::from(([127, 0, 0, 1], peer.port)));

    // The loop obeys this crate's own rule: `hub.next()` is never wrapped in a `timeout`
    // or a `select!`, because the flush inside it is not cancel-safe. The deadline is
    // enforced with `wake_at`, which is what that rule says to use instead — and writing
    // it the other way first is how easy the mistake is.
    let mut limited = None;
    let started = std::time::Instant::now();
    let budget = Duration::from_secs(60);
    while started.elapsed() < budget && limited.is_none() {
        let event = match hub.next().await {
            Ok(event) => event,
            Err(e) => panic!("the session ended after {:?}: {e}", started.elapsed()),
        };
        let now = hub.now();
        match event {
            HubEvent::Spine(event) => {
                if let Some(CsEvent::LimitDecided { write, outcome, .. }) =
                    actor.handle_event(hub.engine_mut(), &event, now)
                    && outcome.is_accepted()
                {
                    limited = Some(write);
                }
            }
            HubEvent::Tick => {
                actor.handle_timeout(hub.engine_mut(), now);
            }
            HubEvent::Disconnected { reason, .. } => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                panic!("the control box hung up: {reason:?}\n--- peer log ---\n{logs}");
            }
            HubEvent::HandshakeFailed { error, .. } => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                panic!("the control box refused the connection: {error}\n--- peer log ---\n{logs}");
            }
            _ => {}
        }
        let next = hub.now() + Duration::from_millis(200);
        hub.wake_at(next);
    }

    let limit = limited.unwrap_or_else(|| {
        let logs = docker(&["logs", &peer.id]).unwrap_or_default();
        panic!("no limit arrived from eebus-go within the deadline.\n--- peer log ---\n{logs}");
    });

    println!("eebus-go wrote {limit:?}; the heat pump accepted it");
    assert_eq!(
        actor.system().state(),
        LimitationState::Limited,
        "a limit was accepted but the state machine did not move"
    );
    assert!(
        limit.watts > 0.0,
        "eebus-go's limit came through as {} W",
        limit.watts
    );

    // The §14a record, produced against a real Energy Guard.
    assert!(
        actor.audit().records().next().is_some(),
        "nothing was recorded for the operator"
    );
}

/// A control box with the two features LPC asks of an Energy Guard.
fn control_box() -> (Engine, EnergyGuardActor) {
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

    let guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, Duration::ZERO);
    (engine, guard)
}

/// This crate writes a §14a limit and `eebus-go` applies it.
///
/// The other direction, and the more demanding one: the 2026 implementation guides spend
/// most of their pages on the *controlling* side, and until this test the Energy Guard had
/// only ever been exercised against a Controllable System written by the same hands.
///
/// Behind `guard.require(…)`: a heartbeat immediately before the write and only once the
/// peer has subscribed to it (§2.11), an opening write as soon as the bindings settle,
/// never a deactivation first after a reconnection (§2.13), never a zero duration on an
/// activated limit (§2.2), a refusal retried a minute later (§2.5), and no more than one
/// write every five minutes otherwise (§2.10). All of it now against `eebus-go`'s own
/// `examples/evse`, which approves any write it is asked to approve.
#[tokio::test(flavor = "multi_thread")]
async fn this_crate_writes_a_limit_and_eebus_go_applies_it() {
    if Command::new("docker").arg("info").output().is_err() {
        eprintln!("skipping: docker is not available");
        return;
    }

    let identity =
        cert::self_signed(CertParams::new("i:46925_ControlBox-1")).expect("a certificate");
    let trust = TrustStore::new();
    let node = Node::new(
        "i:46925_ControlBox-1",
        ShipTls::new(identity),
        trust.clone(),
    );
    let ours = node.ski();

    let peer = start_peer("evse", &ours).expect("the EVSE started");
    println!("EVSE SKI {} on port {}", peer.ski, peer.port);
    trust.trust(peer.ski);

    let (engine, mut guard) = control_box();
    let mut hub = Hub::new(node, engine);
    hub.dial(std::net::SocketAddr::from(([127, 0, 0, 1], peer.port)));

    let mut accepted = None;
    let started = std::time::Instant::now();
    let budget = Duration::from_secs(90);
    let mut asked = false;
    let mut peer_device = None;

    while started.elapsed() < budget && accepted.is_none() {
        let event = match hub.next().await {
            Ok(event) => event,
            Err(e) => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                let elapsed = started.elapsed();
                panic!("the session ended after {elapsed:?}: {e}\n--- peer log ---\n{logs}");
            }
        };
        let now = hub.now();
        let mut reported = Vec::new();
        match event {
            HubEvent::PeerDiscovered { device, .. } => {
                peer_device = Some(device);
            }
            HubEvent::Spine(event) => {
                reported.extend(guard.handle_event(hub.engine_mut(), &event, now));
            }
            HubEvent::Tick => {
                reported.extend(guard.handle_timeout(hub.engine_mut(), now));
            }
            HubEvent::Disconnected { reason, .. } => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                panic!("the EVSE hung up: {reason:?}\n--- peer log ---\n{logs}");
            }
            HubEvent::HandshakeFailed { error, .. } => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                panic!("the EVSE refused the connection: {error}\n--- peer log ---\n{logs}");
            }
            _ => {}
        }
        for event in reported {
            // The *active* one. §2.11 has the guard open with a limit as soon as the
            // bindings settle, whether or not the grid is asking for anything, and that
            // opening write is a deactivation — so the first acceptance is not the one
            // this test is about.
            if let GuardEvent::LimitAccepted { limit, .. } = event
                && limit.is_active
            {
                accepted = Some(limit);
            }
        }

        // The grid is asking for 4.2 kW, and that is the only thing this test says. The
        // guard attaches itself to a peer that announces the Controllable System, and
        // everything §2.11 requires before a limit reaches the wire is its problem.
        if !asked && let Some(device) = peer_device.clone() {
            let now = hub.now();
            guard.require(&device, Some(LimitWrite::active(4_200.0)), now);
            asked = true;
        }

        let next = hub.now() + Duration::from_millis(200);
        hub.wake_at(next);
    }

    assert!(
        asked,
        "the peer never announced the Controllable System, so nothing was asked of it"
    );
    let limit = accepted.unwrap_or_else(|| {
        let logs = docker(&["logs", &peer.id]).unwrap_or_default();
        panic!("eebus-go never accepted a limit.\n--- peer log ---\n{logs}");
    });

    println!("eebus-go accepted {limit:?} from this crate");
    assert!(limit.is_active, "the accepted limit was not an active one");
    assert_eq!(limit.watts, 4_200.0);

    // The §14a record, produced against a real Controllable System.
    assert!(
        guard.audit().records().next().is_some(),
        "nothing was recorded for the operator"
    );
}

/// [F4] The other direction: **this crate listens** and `eebus-go` dials in.
///
/// Both tests above have this crate dialling a container that listens, and that is the
/// wrong way round for the installation §14a actually describes: the control box goes to
/// the household appliance, and the appliance is the one holding a listener open. A test
/// suite that only ever dials has never exercised its own accept path against anything it
/// did not write — which is the blind spot this closes, and the same one a consumer has one
/// level up when both ends of its session test are the same crate.
///
/// Two things have to be true for `eebus-go` to reach a listener outside its container,
/// and neither is Docker's default:
///
/// * **mDNS has to cross the boundary.** The `eebus-go` examples take `-remoteski` and
///   then *discover* where that SKI is; there is no address to give them. Multicast does
///   not traverse Docker's bridge, so the container needs the host's own network stack.
/// * **The announcement has to exist.** `Hub::listen` opens a socket and says nothing;
///   this crate announces `_ship._tcp` through [`eebus::mdns`], and without that the peer
///   has nothing to find.
///
/// `--network host` gives the first, and is honoured only by a native Linux daemon. On
/// Docker Desktop the container is inside a VM: `host.docker.internal` reaches the VM's
/// gateway, which is enough for a TCP dial to a known address and not enough for
/// discovery. The test says which case it skipped for rather than reporting a pass.
#[cfg(feature = "mdns")]
#[tokio::test(flavor = "multi_thread")]
async fn eebus_go_dials_this_crate_listening() {
    use eebus::mdns::Mdns;
    use eebus::ship::{ShipId, ShipTxtRecord};

    if Command::new("docker").arg("info").output().is_err() {
        eprintln!("skipping: docker is not available");
        return;
    }
    if !has_host_networking() {
        eprintln!(
            "skipping: `--network host` does not share the host's network on this daemon, \
             so the container's mDNS cannot see a listener on it. Run this on a native \
             Linux Docker daemon."
        );
        return;
    }

    let ship_id = "i:46925_HeatPump-2";
    let identity = cert::self_signed(CertParams::new(ship_id)).expect("a certificate");
    let trust = TrustStore::new();
    let node = Node::new(ship_id, ShipTls::new(identity), trust.clone());
    let ours = node.ski();

    let (engine, actor) = heat_pump();
    let mut hub = Hub::new(node, engine);
    let address = hub.listen("0.0.0.0:4712").await.expect("a listener");
    let mut actor = actor.install(hub.engine_mut(), Duration::ZERO);

    // The announcement, without which `-remoteski` has nothing to resolve.
    let mut mdns = Mdns::new().expect("mDNS is available");
    let record = ShipTxtRecord::new(ship_id.parse::<ShipId>().expect("a SHIP ID"), ours);
    mdns.announce(ship_id, &record, address.port(), &[])
        .expect("the announcement went out");

    let peer = start_peer_on_host_network("controlbox", &ours).expect("the control box started");
    println!("control box SKI {} dialling us on {address}", peer.ski);
    trust.trust(peer.ski);

    let mut limited = None;
    let started = std::time::Instant::now();
    // Longer than the dialling tests: the peer has to discover us first, and mDNS
    // announcements are not instantaneous.
    let budget = Duration::from_secs(120);
    while started.elapsed() < budget && limited.is_none() {
        let event = match hub.next().await {
            Ok(event) => event,
            Err(e) => panic!("the session ended after {:?}: {e}", started.elapsed()),
        };
        let now = hub.now();
        match event {
            HubEvent::Spine(event) => {
                if let Some(CsEvent::LimitDecided { write, outcome, .. }) =
                    actor.handle_event(hub.engine_mut(), &event, now)
                    && outcome.is_accepted()
                {
                    limited = Some(write);
                }
            }
            HubEvent::Tick => {
                actor.handle_timeout(hub.engine_mut(), now);
            }
            HubEvent::HandshakeFailed { error, .. } => {
                let logs = docker(&["logs", &peer.id]).unwrap_or_default();
                panic!("the control box was refused: {error}\n--- peer log ---\n{logs}");
            }
            _ => {}
        }
        let next = hub.now() + Duration::from_millis(200);
        hub.wake_at(next);
    }
    let _ = mdns.withdraw();

    let limit = limited.unwrap_or_else(|| {
        let logs = docker(&["logs", &peer.id]).unwrap_or_default();
        panic!(
            "eebus-go never found us. It discovers its peer over mDNS, so the usual cause \
             is that the announcement did not reach the container.\n--- peer log ---\n{logs}"
        );
    });

    println!("eebus-go dialled in and wrote {limit:?}");
    assert_eq!(
        actor.system().state(),
        LimitationState::Limited,
        "a limit was accepted but the state machine did not move"
    );
    assert!(limit.watts > 0.0);
}
