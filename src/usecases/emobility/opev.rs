//! Overload Protection by EV Charging Current Curtailment (OPEV).
//!
//! An energy manager holds a car's charging current below what the supply can carry, and
//! the car falls back to a safe current the moment it stops hearing from the manager.
//! Three scenarios, all mandatory for both actors: the curtailment itself, a heartbeat the
//! car watches, and an error state that means the same thing as a missing heartbeat.
//!
//! This module carries what is specific to OPEV — the descriptors, and the [`PURPOSE`] an
//! actor is built with. The state machine, the payloads and both actors live in
//! [`charging`](super::charging), because [`oscev`](super::oscev) is the same exchange
//! for the opposite reason and one implementation serving both is one implementation to
//! get right.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::ElectricalConnectionPhaseName as Phase;
//! use eebus::usecases::emobility::charging::{ChargingCurrents, EvCharging};
//! use eebus::usecases::emobility::opev;
//!
//! // A car that will fall back to 6 A per phase if left alone.
//! let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
//! let now = Duration::from_secs(1);
//! ev.on_heartbeat(now);
//! ev.on_limit(ChargingCurrents::new(16.0, 10.0, 10.0), now);
//! assert_eq!(ev.effective(Phase::A), Some(16.0));
//!
//! // Four seconds of silence and the safe current is back. [OPEV-005]
//! ev.handle_timeout(now + Duration::from_secs(5));
//! assert_eq!(ev.effective(Phase::A), Some(6.0));
//! assert_eq!(opev::PURPOSE.limit_category().as_str(), "obligation");
//! ```

use crate::usecases::descriptor::{ActorRole, Scenario, Support, UseCaseDescriptor};

use super::charging::{
    EV_ENTITIES, EV_FUNCTIONS, EV_WATCHES, GUARD_ENTITIES, GUARD_SERVES, GUARD_WRITES, Purpose,
    VERSION,
};
use super::{actors, names};

/// The purpose an actor of this use case is built with.
pub const PURPOSE: Purpose = Purpose::OverloadProtection;

/// The car: the actor whose current is curtailed.
pub static EV: UseCaseDescriptor = UseCaseDescriptor {
    name: names::OPEV,
    actor: actors::EV,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EV_ENTITIES,
    counterpart: actors::CEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Energy Guard curtails charging current of EV",
            support: Support::Mandatory,
            functions: EV_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "EV checks Energy Guard availability",
            support: Support::Mandatory,
            functions: EV_WATCHES,
        },
        Scenario {
            number: 3,
            name: "Energy Guard sends error state",
            support: Support::Mandatory,
            functions: EV_WATCHES,
        },
    ],
};

/// The energy manager: the actor that curtails.
pub static ENERGY_GUARD: UseCaseDescriptor = UseCaseDescriptor {
    name: names::OPEV,
    actor: actors::CEM,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: GUARD_ENTITIES,
    counterpart: actors::EV,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Energy Guard curtails charging current of EV",
            support: Support::Mandatory,
            functions: GUARD_WRITES,
        },
        Scenario {
            number: 2,
            name: "EV checks Energy Guard availability",
            support: Support::Mandatory,
            functions: GUARD_SERVES,
        },
        Scenario {
            number: 3,
            name: "Energy Guard sends error state",
            support: Support::Mandatory,
            functions: GUARD_SERVES,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FeatureType, LoadControlCategory, ScopeType};

    #[test]
    fn the_descriptors_say_what_the_specification_says() {
        assert_eq!(EV.use_case_name().as_str(), names::OPEV);
        assert_eq!(EV.use_case_actor().as_str(), "EV");
        assert_eq!(ENERGY_GUARD.use_case_actor().as_str(), "CEM");
        assert_eq!(EV.version, "1.0.1");

        // Table 1: all three scenarios mandatory for both actors.
        assert_eq!(
            EV.required_scenarios().collect::<alloc::vec::Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            ENERGY_GUARD
                .required_scenarios()
                .collect::<alloc::vec::Vec<_>>(),
            [1, 2, 3]
        );

        // The Energy Guard writes the limit, so it binds to `LoadControl` and nothing else.
        assert_eq!(
            ENERGY_GUARD
                .features_needing_binding()
                .collect::<alloc::vec::Vec<_>>(),
            [&FeatureType::LoadControl]
        );
    }

    /// Table 6: the pair that tells a car the ceiling is a fuse and not an offer.
    #[test]
    fn opev_001_the_limit_is_an_obligation_for_overload_protection() {
        assert_eq!(PURPOSE.limit_category(), LoadControlCategory::Obligation);
        assert_eq!(PURPOSE.scope_type(), ScopeType::OverloadProtection);
        assert_eq!(PURPOSE.use_case_name(), names::OPEV);
    }
}
