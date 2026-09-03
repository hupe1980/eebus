//! A whole datagram through the SPINE engine, not just the parser.
//!
//! Decoding a datagram is one thing; routing it, checking permission, merging a partial
//! write into stored data and deciding what to answer is where the interesting state
//! lives. This target drives all of it against a device that has something to write to.
#![no_main]

use core::time::Duration;

use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, Operations};
use libfuzzer_sys::fuzz_target;

fn heat_pump() -> Engine {
    let mut device =
        LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(
                    LocalFeature::new(1, FeatureType::LoadControl, Role::Server)
                        .with_function(
                            Function::LoadControlLimitDescriptionListData,
                            Operations::read(),
                        )
                        .with_function(
                            Function::LoadControlLimitListData,
                            Operations::read_write(),
                        ),
                )
                .with_feature(
                    LocalFeature::new(2, FeatureType::Measurement, Role::Server)
                        .with_function(Function::MeasurementListData, Operations::read_write()),
                ),
        )
        .unwrap();

    let mut engine = Engine::new(device);
    // A binding, so writes get past the permission check and reach the merge.
    let client = eebus::spine::feature_address(
        &eebus::spine::device_address("i:12345", "ControlBox-1").unwrap(),
        &[1],
        1,
    );
    for feature in [1, 2] {
        let server = engine.device().address_of(&[1], feature);
        engine.insert_binding(&client, &server);
        engine.insert_subscription(&client, &server);
    }
    engine
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    // Several datagrams in one input, so state built by one reaches the next.
    let mut engine = heat_pump();
    let mut now = Duration::ZERO;

    for line in text.split('\n').take(16) {
        let Ok(datagram) = eebus::model::from_json_str(line) else {
            continue;
        };
        engine.handle_datagram(&datagram, now);
        now += Duration::from_millis(100);
        engine.handle_timeout(now);

        while let Some(event) = engine.poll_event() {
            // A deferred write has to be resolved or the engine leaks it.
            if let eebus::spine::SpineEvent::WriteRequested(write) = event {
                let _ = engine.accept_write(write.token, now);
            }
        }
        // Everything queued must be encodable: a datagram the engine produces and cannot
        // write is one that would abort a real connection.
        while let Some(out) = engine.poll_transmit() {
            eebus::model::to_json(&out).expect("the engine produced an unencodable datagram");
        }
    }
});
