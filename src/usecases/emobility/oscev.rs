//! Optimization of Self-Consumption During EV Charging (OSCEV).
//!
//! The mirror image of [`opev`](super::opev), and the reason the two share an
//! implementation. An energy manager tells a car how much *self-produced* current is going
//! spare — the roof is generating more than the house is using — and the car may take it.
//! Three scenarios, all mandatory for both actors: the recommendation itself, a heartbeat
//! the car watches, and an error state.
//!
//! The wire is nearly identical to OPEV: the same per-phase ceilings in amperes on the same
//! `LoadControl` feature, the same four-second heartbeat, the same fallback. Two elements
//! differ, and a car that confuses them behaves badly in a way that is hard to see:
//!
//! * `limitCategory` is **`recommendation`**, not `obligation`. A car that reads it as an
//!   obligation will throttle itself to the solar surplus and stop charging when a cloud
//!   passes — having been *offered* free power, not *limited* to it.
//! * `scopeType` is **`selfConsumption`**, not `overloadProtection`.
//!
//! Both use cases may run over the same connection at the same time, which is the usual
//! installation: the fuse says what the car may never exceed, and the roof says what is
//! cheap right now. The obligation always wins, and it wins in the car.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::ElectricalConnectionPhaseName as Phase;
//! use eebus::usecases::emobility::charging::{ChargingCurrents, EvCharging};
//! use eebus::usecases::emobility::oscev;
//!
//! // The same machine as OPEV, pointed at the other reason.
//! let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
//! let now = Duration::from_secs(1);
//! ev.on_heartbeat(now);
//! ev.on_limit(ChargingCurrents::same(12.0), now);
//! assert_eq!(ev.effective(Phase::A), Some(12.0), "the surplus on offer");
//!
//! assert_eq!(oscev::PURPOSE.limit_category().as_str(), "recommendation");
//! assert_eq!(oscev::PURPOSE.scope_type().as_str(), "selfConsumption");
//! ```

use crate::usecases::descriptor::{ActorRole, Scenario, Support, UseCaseDescriptor};

use super::charging::{
    EV_ENTITIES, EV_FUNCTIONS, EV_WATCHES, GUARD_ENTITIES, GUARD_SERVES, GUARD_WRITES, Purpose,
    VERSION,
};
use super::{actors, names};

/// The purpose an actor of this use case is built with.
pub const PURPOSE: Purpose = Purpose::SelfConsumption;

/// The car: the actor that is offered the surplus.
pub static EV: UseCaseDescriptor = UseCaseDescriptor {
    name: names::OSCEV,
    actor: actors::EV,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EV_ENTITIES,
    counterpart: actors::CEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: "CEM informs EV about self-produced current",
            support: Support::Mandatory,
            functions: EV_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "EV checks CEM availability",
            support: Support::Mandatory,
            functions: EV_WATCHES,
        },
        Scenario {
            number: 3,
            name: "CEM sends error state",
            support: Support::Mandatory,
            functions: EV_WATCHES,
        },
    ],
};

/// The energy manager: the actor that offers the surplus.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: names::OSCEV,
    actor: actors::CEM,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: GUARD_ENTITIES,
    counterpart: actors::EV,
    scenarios: &[
        Scenario {
            number: 1,
            name: "CEM informs EV about self-produced current",
            support: Support::Mandatory,
            functions: GUARD_WRITES,
        },
        Scenario {
            number: 2,
            name: "EV checks CEM availability",
            support: Support::Mandatory,
            functions: GUARD_SERVES,
        },
        Scenario {
            number: 3,
            name: "CEM sends error state",
            support: Support::Mandatory,
            functions: GUARD_SERVES,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LoadControlCategory, ScopeType};

    #[test]
    fn the_descriptors_say_what_the_specification_says() {
        assert_eq!(EV.use_case_name().as_str(), names::OSCEV);
        assert_eq!(CEM.use_case_actor().as_str(), "CEM");

        // Table 1: all three scenarios mandatory for both actors, as in OPEV.
        assert_eq!(
            EV.required_scenarios().collect::<alloc::vec::Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            CEM.required_scenarios().collect::<alloc::vec::Vec<_>>(),
            [1, 2, 3]
        );
    }

    /// [OSCEV-001] and Table 6: an offer, not a fuse. The one thing a car must not
    /// confuse with OPEV, because confusing them means charging on solar only.
    #[test]
    fn oscev_001_the_limit_is_a_recommendation_for_self_consumption() {
        assert_eq!(
            PURPOSE.limit_category(),
            LoadControlCategory::Recommendation
        );
        assert_eq!(PURPOSE.scope_type(), ScopeType::SelfConsumption);
        assert_eq!(PURPOSE.use_case_name(), names::OSCEV);

        // And the two use cases are told apart by exactly those two elements.
        assert_ne!(
            PURPOSE.limit_category(),
            super::super::opev::PURPOSE.limit_category()
        );
        assert_ne!(
            PURPOSE.scope_type(),
            super::super::opev::PURPOSE.scope_type()
        );
    }
}
