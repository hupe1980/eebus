//! Datagrams from devices this crate has never met.
//!
//! Every other test here exercises `eebus` against itself: the round trips prove the
//! encoder agrees with the decoder, which is not the question anyone is asking. These
//! fifteen datagrams were captured from real hardware by somebody else's implementation —
//! `enbility/devices`, recorded with `eebus-go` — and converted to the wire format by
//! `cargo xtask devices`.
//!
//! Eight devices from seven manufacturers, answering the only two questions a SHIP
//! connection opens with: *what are you?* and *what do you do?*

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use eebus::model::{CmdData, Datagram, from_json_str, to_json};

/// Every captured datagram, as (name, wire JSON).
fn corpus() -> Vec<(String, String)> {
    let dir = Path::new("tests/fixtures/devices");
    let mut out: Vec<(String, String)> = fs::read_dir(dir)
        .expect("the device corpus is committed")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, fs::read_to_string(&path).ok()?))
        })
        .collect();
    out.sort();
    assert!(out.len() >= 15, "the corpus shrank: {}", out.len());
    out
}

fn parse(name: &str, wire: &str) -> Datagram {
    match from_json_str(wire) {
        Ok(datagram) => datagram,
        Err(e) => panic!("{name} does not parse: {e}"),
    }
}

/// Every captured datagram parses, and re-encoding is stable.
///
/// **Stable, not byte-for-byte** — and the difference is a real finding rather than a
/// weakened assertion. The byte-for-byte guarantee the crate makes holds for
/// schema-valid messages, which is what `tests/fixtures/spine` contains. One device here
/// is not schema-valid: `evcc` (built on `eebus-go`) puts a `device` inside the
/// `entityAddress` and `featureAddress` of detailed discovery, where SPINE 1.3.0 defines
/// a *restricted* address type carrying only the entity path. This crate's model is
/// generated from that schema, so the field has nowhere to go and is dropped.
///
/// Dropping it is the right answer — it is redundant, since the enclosing
/// `deviceInformation` names the device, and refusing would mean refusing to talk to
/// `evcc` — but it means "parse and re-encode" is not the identity for real traffic. What
/// *is* invariant is that re-encoding and parsing again yields the same model: nothing
/// degrades further on a second pass, so a datagram forwarded through this crate is
/// stable rather than progressively lossy.
#[test]
fn every_real_device_datagram_re_encodes_stably() {
    for (name, wire) in corpus() {
        let once = parse(&name, &wire);
        let out = to_json(&once).unwrap_or_else(|e| panic!("{name} does not re-encode: {e}"));
        let twice = from_json_str(&out)
            .unwrap_or_else(|e| panic!("{name} does not survive its own encoding: {e}"));
        assert_eq!(once, twice, "{name} is not stable under re-encoding");
    }
}

/// And the one place a real device says more than the schema allows is *known*, not
/// merely tolerated.
///
/// If a future corpus entry loses something else, this test is what says so: every
/// capture but the known one is byte-for-byte, and the known one differs only there.
#[test]
fn the_only_lossy_capture_is_the_one_that_is_not_schema_valid() {
    let mut lossy = Vec::new();
    for (name, wire) in corpus() {
        let datagram = parse(&name, &wire);
        let out = to_json(&datagram).expect("re-encode");
        if out.trim() != wire.trim() {
            lossy.push(name);
        }
    }
    assert_eq!(
        lossy,
        vec!["evcc.io_evcc.discovery.json".to_string()],
        "the set of captures this crate cannot reproduce exactly has changed"
    );
}

/// The header of every capture is one this crate would accept off a socket.
#[test]
fn every_real_device_header_is_understood() {
    for (name, wire) in corpus() {
        let datagram = parse(&name, &wire);
        let header = datagram
            .header
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no header"));

        let version = header
            .specification_version
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no specificationVersion"));
        let parsed = eebus::spine::SpecVersion::parse(version.as_str())
            .unwrap_or_else(|| panic!("{name}: `{version}` is not a version this crate reads"));
        assert!(
            parsed.is_compatible_with(eebus::spine::SUPPORTED),
            "{name}: {version} is not compatible"
        );

        let source = header
            .address_source
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no addressSource"));
        assert!(
            source.device.is_some(),
            "{name}: a reply has to name the device it came from"
        );
    }
}

/// Detailed discovery from a real device resolves into entities and features.
#[test]
fn real_discovery_resolves_into_a_device_model() {
    let mut seen_feature_types = BTreeSet::new();
    let mut devices = 0;

    for (name, wire) in corpus() {
        if !name.contains(".discovery.") {
            continue;
        }
        let datagram = parse(&name, &wire);
        let cmd = &datagram
            .payload
            .as_ref()
            .and_then(|p| p.cmd.as_ref())
            .unwrap_or_else(|| panic!("{name}: no cmd"))[0];
        let Some(CmdData::NodeManagementDetailedDiscoveryData(discovery)) = &cmd.data else {
            panic!("{name}: the payload is not detailed discovery");
        };

        let entities = discovery
            .entity_information
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no entities"));
        assert!(!entities.is_empty(), "{name}: an empty entity list");

        let features = discovery
            .feature_information
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no features"));
        assert!(!features.is_empty(), "{name}: an empty feature list");

        for feature in features {
            let description = feature
                .description
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: a feature with no description"));
            if let Some(kind) = &description.feature_type {
                seen_feature_types.insert(format!("{kind:?}"));
            }
        }
        devices += 1;
    }

    assert!(devices >= 8, "only {devices} discovery captures");
    // A corpus that only ever exercised `Generic` would prove nothing about the model.
    assert!(
        seen_feature_types.len() >= 8,
        "the corpus covers only {} feature types: {seen_feature_types:?}",
        seen_feature_types.len()
    );
}

/// Use-case discovery from a real device resolves into named use cases and actors.
#[test]
fn real_use_case_data_resolves_into_use_cases() {
    let mut seen = BTreeSet::new();

    for (name, wire) in corpus() {
        if !name.contains(".usecase.") {
            continue;
        }
        let datagram = parse(&name, &wire);
        let cmd = &datagram
            .payload
            .as_ref()
            .and_then(|p| p.cmd.as_ref())
            .unwrap_or_else(|| panic!("{name}: no cmd"))[0];
        let Some(CmdData::NodeManagementUseCaseData(data)) = &cmd.data else {
            panic!("{name}: the payload is not use-case data");
        };

        for information in data.use_case_information.as_ref().unwrap_or(&Vec::new()) {
            for support in information.use_case_support.as_ref().unwrap_or(&Vec::new()) {
                if let Some(use_case) = &support.use_case_name {
                    seen.insert(format!("{use_case:?}"));
                }
            }
        }
    }

    // The four certifiable ones are what this crate exists for; a corpus of real devices
    // that named none of them would say the corpus was the wrong corpus.
    // The four certifiable ones are what this crate exists for; a corpus of real devices
    // naming none of them would say the corpus was the wrong corpus.
    for expected in [
        "limitationOfPowerConsumption",
        "limitationOfPowerProduction",
        "monitoringOfPowerConsumption",
        "monitoringOfGridConnectionPoint",
    ] {
        assert!(
            seen.iter().any(|u| u.contains(expected)),
            "no device in the corpus plays {expected}: {seen:?}"
        );
    }
    assert!(
        seen.len() >= 12,
        "only {} use cases named: {seen:?}",
        seen.len()
    );
}

/// What real devices play that this crate does not implement.
///
/// The corpus is a demand signal as well as a correctness check, and this is the test that
/// keeps it visible. Every use case a captured device names is either implemented here or
/// listed below with the number of devices playing it — so a corpus that grows a new
/// unimplemented use case fails this test rather than quietly making the roadmap stale.
///
/// The sample is small and skewed: five of the eight devices are chargers, which is why
/// the e-mobility family dominates and LPC appears on only two. It is evidence, not a
/// market survey.
#[test]
fn the_corpus_demand_signal_is_current() {
    use std::collections::BTreeMap;

    let implemented = [
        "limitationOfPowerConsumption",
        "limitationOfPowerProduction",
        "monitoringOfPowerConsumption",
        "monitoringOfGridConnectionPoint",
        "evseCommissioningAndConfiguration",
        "evCommissioningAndConfiguration",
        "overloadProtectionByEvChargingCurrentCurtailment",
        "optimizationOfSelfConsumptionDuringEvCharging",
        "measurementOfElectricityDuringEvCharging",
        "evStateOfCharge",
        "monitoringOfInverter",
        "monitoringOfPvString",
        "monitoringOfBattery",
        "controlOfBattery",
    ];

    // Use case -> how many devices in the corpus play it.
    let mut plays: BTreeMap<String, usize> = BTreeMap::new();
    for (name, wire) in corpus() {
        if !name.contains(".usecase.") {
            continue;
        }
        let datagram = parse(&name, &wire);
        let mut here = BTreeSet::new();
        let cmd = &datagram
            .payload
            .as_ref()
            .and_then(|p| p.cmd.as_ref())
            .expect("a cmd")[0];
        if let Some(CmdData::NodeManagementUseCaseData(data)) = &cmd.data {
            for information in data.use_case_information.as_ref().unwrap_or(&Vec::new()) {
                for support in information.use_case_support.as_ref().unwrap_or(&Vec::new()) {
                    if let Some(use_case) = &support.use_case_name {
                        here.insert(use_case.as_str().to_string());
                    }
                }
            }
        }
        for use_case in here {
            *plays.entry(use_case).or_default() += 1;
        }
    }

    let mut unimplemented: Vec<(usize, String)> = plays
        .into_iter()
        .filter(|(name, _)| !implemented.contains(&name.as_str()))
        .map(|(name, count)| (count, name))
        .collect();
    unimplemented.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    // A new entry here is a use case real hardware plays and this crate does not
    // implement — worth knowing before a consumer discovers it.
    let expected: Vec<(usize, String)> = vec![
        (4, "coordinatedEvCharging".into()),
        (4, "evChargingSummary".into()),
        (1, "flexibleStartForWhiteGoods".into()),
        (1, "monitoringAndControlOfSmartGridReadyConditions".into()),
        (
            1,
            "optimizationOfSelfConsumptionByHeatPumpCompressorFlexibility".into(),
        ),
        (1, "visualizationOfAggregatedBatteryData".into()),
        (1, "visualizationOfAggregatedPhotovoltaicData".into()),
    ];
    assert_eq!(
        unimplemented, expected,
        "the corpus now plays a different set of use cases this crate does not implement; \
         update the backlog and then this list"
    );
}

/// **Every capture is *served* to an engine, not only parsed.**
///
/// The tests above read the payload structure by hand, which proves the codec and nothing
/// above it. The engine is what will actually consume a real device's answer — it is
/// `Engine::handle_datagram` that files a reply under a peer, and
/// `RemoteDevice::apply_detailed_discovery` that turns it into entities and features —
/// and until this test, no capture had ever been through it.
///
/// The distinction is not academic: the one capture that is not schema-valid is
/// `eebus-go`'s, which is the implementation most of the deployed base is built on, and
/// "the model survives" is a different claim from "the payload decodes".
#[test]
fn every_capture_is_resolved_by_an_engine() {
    use core::time::Duration;
    use eebus::model::{DeviceType, EntityType};
    use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent};

    let mut resolved_devices = 0;
    let mut resolved_use_cases = 0;

    for (name, wire) in corpus() {
        let datagram = parse(&name, &wire);
        let header = datagram.header.as_ref().expect("a header");
        let peer = header
            .address_source
            .as_ref()
            .and_then(|a| a.device.clone())
            .unwrap_or_else(|| panic!("{name}: a capture with no source device"));

        // The engine refuses a datagram addressed elsewhere, which is right — so the
        // local device takes the address the capture was sent to. Everything else about
        // the message is the device's own.
        let destination = header
            .address_destination
            .as_ref()
            .and_then(|a| a.device.clone())
            .unwrap_or_else(|| panic!("{name}: a capture with no destination device"));
        let mut device = LocalDevice::from_address(destination, DeviceType::EnergyManagementSystem);
        device
            .add_entity(LocalEntity::new([1], EntityType::CEM))
            .expect("one entity");
        let mut engine = Engine::new(device);

        assert!(
            engine.handle_datagram(&datagram, Duration::ZERO),
            "{name}: the engine discarded a real device's answer"
        );
        assert!(
            core::iter::from_fn(|| engine.poll_transmit())
                .next()
                .is_none(),
            "{name}: a reply or notification was answered with something"
        );

        let events: Vec<_> = core::iter::from_fn(|| engine.poll_event()).collect();
        let remote = engine
            .peer(&peer)
            .unwrap_or_else(|| panic!("{name}: the peer was not recorded"));

        if name.contains(".discovery.") {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, SpineEvent::DiscoveryUpdated { .. })),
                "{name}: no discovery event"
            );
            assert!(
                !remote.entities.is_empty(),
                "{name}: the engine resolved no entities"
            );
            assert!(
                remote.entities.iter().any(|e| !e.features.is_empty()),
                "{name}: the engine resolved no features"
            );
            resolved_devices += 1;
        } else if name.contains(".usecase.") {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, SpineEvent::UseCasesUpdated { .. })),
                "{name}: no use-case event"
            );
            assert!(
                !remote.use_cases.is_empty(),
                "{name}: the engine resolved no use cases"
            );
            resolved_use_cases += 1;
        }
    }

    assert!(
        resolved_devices >= 8,
        "only {resolved_devices} devices resolved through an engine"
    );
    assert!(
        resolved_use_cases >= 7,
        "only {resolved_use_cases} use-case tables resolved through an engine"
    );
}

/// **Two of these seven devices name no address for their use cases at all**, and the
/// features they need still resolve.
///
/// SPINE 1.1.1's `useCaseInformation` had no `address` element; it arrived later and is
/// optional. `eebus-go` still omits it, so evcc does — and so does Porsche's Mobile
/// Charger Connect, a shipping wallbox that announces itself as the **EV** actor of
/// `overloadProtectionByEvChargingCurrentCurtailment`, which is a use case this crate
/// implements both halves of.
///
/// Before D67 the engine required the address and dropped the whole entry without it, so
/// both devices resolved to *zero* use cases and neither could ever have been located.
/// The count is asserted in both directions: a third such device is worth noticing, and so
/// is one of these two starting to send an address.
#[test]
fn a_use_case_without_an_address_still_locates_its_features() {
    use core::time::Duration;
    use eebus::model::{DeviceType, EntityType, FeatureType, Role};
    use eebus::spine::{Engine, LocalDevice, LocalEntity, RemoteDevice};

    /// Feeds a device's discovery and use-case captures to one engine.
    ///
    /// **Both device addresses are normalised first, and that is an artefact of the
    /// capture rather than of the protocol.** The two recordings of one device come from
    /// different sessions, and the corpus anonymises the serial in each independently —
    /// the Porsche answers as `PorscheEVSE-xxxxxxxx` in one and `PorscheEVSE-00009463` in
    /// the other. To an engine those are two different peers, which is exactly right and
    /// exactly not what this test is about. So the destination becomes the engine's own
    /// address and the source becomes whichever one arrived first.
    fn resolve(prefix: &str) -> RemoteDevice {
        let mut device =
            LocalDevice::new("i:46925", "Corpus-CEM", DeviceType::EnergyManagementSystem)
                .expect("an address");
        device
            .add_entity(LocalEntity::new([1], EntityType::CEM))
            .expect("one entity");
        let local = device.address().clone();
        let mut engine = Engine::new(device);
        let mut peer = None;

        for (name, wire) in corpus() {
            if !name.starts_with(prefix) {
                continue;
            }
            let mut datagram = parse(&name, &wire);
            let header = datagram.header.as_mut().expect("a header");
            let canonical = peer
                .clone()
                .or_else(|| {
                    header
                        .address_source
                        .as_ref()
                        .and_then(|a| a.device.clone())
                })
                .expect("a source device");
            peer = Some(canonical.clone());
            if let Some(source) = header.address_source.as_mut() {
                source.device = Some(canonical);
            }
            if let Some(destination) = header.address_destination.as_mut() {
                destination.device = Some(local.clone());
            }
            assert!(
                engine.handle_datagram(&datagram, Duration::ZERO),
                "{name}: discarded"
            );
        }
        engine
            .peer(&peer.unwrap_or_else(|| panic!("no captures for {prefix}")))
            .unwrap_or_else(|| panic!("{prefix}: the peer was not recorded"))
            .clone()
    }

    let addressless: Vec<_> = corpus()
        .into_iter()
        .filter(|(name, _)| name.contains(".usecase."))
        .filter(|(name, wire)| {
            let datagram = parse(name, wire);
            let Some(CmdData::NodeManagementUseCaseData(data)) = datagram
                .payload
                .as_ref()
                .and_then(|p| p.cmd.as_ref())
                .and_then(|c| c.first())
                .and_then(|c| c.data.as_ref())
            else {
                panic!("{name}: not a use-case table");
            };
            data.use_case_information
                .iter()
                .flatten()
                .all(|information| information.address.is_none())
        })
        .map(|(name, _)| name.split(".usecase.").next().unwrap().to_string())
        .collect();

    assert_eq!(
        addressless,
        ["evcc.io_evcc", "porsche_mobile-charger-connect"],
        "the set of devices that name no use-case address changed"
    );

    // evcc is a CEM, so its LoadControl is the client half; the Porsche is the EV, so
    // its LoadControl is the server. Either way the feature is on exactly one entity,
    // which is what makes the fallback well defined.
    for (prefix, role) in [
        ("evcc.io_evcc", Role::Client),
        ("porsche_mobile-charger-connect", Role::Server),
    ] {
        let remote = resolve(prefix);
        assert!(
            !remote.use_cases.is_empty(),
            "{prefix}: no use cases resolved at all"
        );
        let opev = remote
            .use_cases
            .iter()
            .find(|u| u.name.as_str() == "overloadProtectionByEvChargingCurrentCurtailment")
            .unwrap_or_else(|| panic!("{prefix}: OPEV was not resolved"));
        assert!(
            opev.address.is_none(),
            "{prefix}: this one names no address"
        );
        assert!(
            remote
                .address_of(opev, &FeatureType::LoadControl, role)
                .is_some(),
            "{prefix}: the LoadControl the use case needs was not located"
        );
        // And where the feature is on several entities the answer is honest rather than
        // a guess: the Porsche carries `DeviceClassification` on three of them.
        if prefix == "porsche_mobile-charger-connect" {
            assert!(
                remote
                    .address_of(opev, &FeatureType::DeviceClassification, Role::Server)
                    .is_none(),
                "an ambiguous feature must not be guessed at"
            );
        }
    }
}

/// Every address in the corpus is usable, and **two of them are not conformant**.
///
/// §7.1.1.2 fixes the shape `d:_(i:<IANA PEN>|n:<vendor>)_<unique>`, and `evcc` — which is
/// `eebus-go`, which is much of the deployed base — announces `d:_i:EVCC_HEMS-…`: the `i:`
/// marker without a PEN behind it. Refusing that means refusing to talk to it, so the crate
/// does what D52 did with the restricted address type — tolerates it and *measures* the
/// gap, so a third instance is a failing test rather than a discovery.
///
/// What is enforced instead is the part that protects this node: an address is a routing
/// key and a stored identity, so it has to be bounded and printable.
#[test]
fn every_real_device_address_is_usable_and_the_deviations_are_named() {
    use eebus::spine::{is_usable_device_address, validate_device_address};

    let mut deviating = BTreeSet::new();
    let mut seen = 0;

    for (name, wire) in corpus() {
        let datagram = parse(&name, &wire);
        let address = datagram
            .header
            .as_ref()
            .and_then(|h| h.address_source.as_ref())
            .and_then(|a| a.device.as_ref())
            .unwrap_or_else(|| panic!("{name}: no source device"));
        seen += 1;

        assert!(
            is_usable_device_address(address.as_str()),
            "{name}: {address:?} cannot be used as a routing key"
        );
        if validate_device_address(address.as_str()).is_err() {
            deviating.insert(address.as_str().to_owned());
        }
    }

    assert!(seen >= 15, "only {seen} captures");
    assert_eq!(
        deviating.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            // `eebus-go`: the `i:` marker without an IANA Private Enterprise Number.
            "d:_i:EVCC_HEMS-xxxxxxxxxxxxxxxx",
            // An artefact of the corpus rather than of the device: the PEN is the part
            // `enbility/devices` anonymises here.
            "d:_i:xxxxxxxx_Kostal-KSEM",
        ],
        "the set of non-conformant device addresses changed"
    );
}

/// An address this node cannot key on is discarded rather than taken up.
#[test]
fn an_unusable_device_address_is_not_taken_up() {
    use core::time::Duration;
    use eebus::model::{DeviceType, EntityType};
    use eebus::spine::{Engine, LocalDevice, LocalEntity};

    let (name, wire) = corpus()
        .into_iter()
        .find(|(name, _)| name.contains(".discovery."))
        .expect("a capture");
    let mut datagram = parse(&name, &wire);
    let header = datagram.header.as_mut().expect("a header");

    let mut device =
        LocalDevice::new("i:46925", "Corpus-CEM", DeviceType::EnergyManagementSystem).unwrap();
    device
        .add_entity(LocalEntity::new([1], EntityType::CEM))
        .unwrap();
    let local = device.address().clone();
    if let Some(destination) = header.address_destination.as_mut() {
        destination.device = Some(local);
    }
    // Longer than §7.1.1.2 permits: a header that could otherwise spend this device's
    // memory once per datagram.
    let overlong = eebus::model::AddressDevice::from(format!("d:_i:1_{}", "x".repeat(300)));
    if let Some(source) = header.address_source.as_mut() {
        source.device = Some(overlong.clone());
    }

    let mut engine = Engine::new(device);
    assert!(
        !engine.handle_datagram(&datagram, Duration::ZERO),
        "an unusable source address was processed"
    );
    assert!(engine.peer(&overlong).is_none(), "and it allocated a peer");
    assert_eq!(engine.peers().count(), 0);
}
