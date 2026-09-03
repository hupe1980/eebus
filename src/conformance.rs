//! The EEBUS High-Level Test Specifications as data: what a certification laboratory
//! will actually run.
//!
//! Certification for LPC, LPP, MPC and MGCP is a laboratory booking, and the bill arrives
//! whether or not the device passes. What the laboratory runs is not a secret — each use
//! case has a High-Level Test Specification listing its *abstract test cases*, each with
//! an identifier, the requirements it covers, which actor it puts under test, and whether
//! it is mandatory. This module is that list, [`CATALOGUE`], transcribed from the four
//! 1.0.2 specifications.
//!
//! Having it as data buys two things:
//!
//! * **A coverage number before the slot is booked.** [`Coverage`] takes the test-case
//!   identifiers a project's own suite exercises and reports what is left. `cargo test`
//!   in this crate does exactly that — see `tests/conformance.rs` — and so can a
//!   consumer's, over its own device.
//! * **A named place to argue from.** A test that says
//!   `ATC_LPC_COM_PT_CSTransition5_001` is checking the same thing the laboratory will,
//!   under the same name, and a reviewer can look it up.
//!
//! What the catalogue deliberately does *not* claim is that a passing test here would
//! pass there. Roughly a third of the abstract test cases are about the device rather
//! than the protocol — that a factory reset restores defaults, that a value survives a
//! power cut, that a reboot completes inside the declared start-up time — and no library
//! can answer those for the device it is linked into. [`AbstractTestCase::level`] and
//! [`AbstractTestCase::raised_when`] say when a test case applies at all; the honest
//! answer for the rest is the reason recorded alongside them in the consuming suite.
//!
//! ```
//! use eebus::conformance::{self, Coverage};
//!
//! use eebus::usecases::descriptor::{Support, actors, names};
//!
//! // Everything a Controllable System must pass for LPC, whatever the device is.
//! let mandatory: Vec<_> = conformance::for_actor(names::LPC, actors::CONTROLLABLE_SYSTEM)
//!     .filter(|tc| tc.level == Support::Mandatory)
//!     .collect();
//!
//! // And what a suite covering two of them is missing.
//! let claimed = ["ATC_LPC_COM_PT_CSTransition5_001", "ATC_LPC_COM_PT_CSTransition7_001"];
//! let report = Coverage::of(mandatory.iter().copied(), &claimed);
//! assert_eq!(report.covered(), 2);
//! assert_eq!(report.missing(), mandatory.len() - 2);
//! ```

use crate::usecases::descriptor::{Support, actors, names};

/// Whether an abstract test case is a positive or a negative test.
///
/// The identifier says which: `PT` for a positive test, which drives the device the way
/// the specification intends and checks that it complies; `NT` for a negative one, which
/// sends something the device must refuse and checks that refusing it does not disturb
/// anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `PT`: the device is asked to do what it should, and must.
    Positive,
    /// `NT`: the device is asked for something invalid, and must decline without harm.
    Negative,
}

/// One abstract test case of a High-Level Test Specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbstractTestCase {
    /// The `ATC_…` identifier, which is what a laboratory report names.
    pub id: &'static str,
    /// The `useCaseName` the test case belongs to, as
    /// [`descriptor::names`](crate::usecases::descriptor::names) spells it.
    pub use_case: &'static str,
    /// The actor under test, as
    /// [`descriptor::actors`](crate::usecases::descriptor::actors) spells it.
    ///
    /// Every abstract test case names exactly one: the coverage table marks the other
    /// actor's column not applicable.
    pub dut: &'static str,
    /// The use-case scenario, where the identifier names one (`SCE1`, `SCE2`, …).
    ///
    /// [`None`] for the common test cases, whose identifiers say `COM`, and for the two
    /// installation-specific ones LPC and LPP mark `INS1`/`INS2`.
    pub scenario: Option<u32>,
    /// Whether the device must comply or must refuse.
    pub kind: Kind,
    /// How the coverage table marks it: `m`, `r` or `o`.
    pub level: Support,
    /// Conditions under which the marking is raised to mandatory.
    ///
    /// The footnotes of the coverage table. A test case marked optional with a condition
    /// here is `o/m`: optional in general, mandatory for a device the condition describes.
    /// Empty means the marking stands as it is.
    pub raised_when: &'static [&'static str],
    /// The requirement identifiers the test case covers, without their brackets.
    pub requirements: &'static [&'static str],
    /// The specification's own one-paragraph description.
    pub description: &'static str,
}

impl AbstractTestCase {
    /// The use case's abbreviation, as the identifier spells it: `LPC`, `MGCP`, …
    pub fn abbreviation(&self) -> &'static str {
        self.id
            .split('_')
            .nth(1)
            .expect("every identifier is ATC_<use case>_…")
    }

    /// Whether this test case is mandatory for a device the conditions describe.
    ///
    /// `conditions` is what the device declares about itself in the parameter sheet —
    /// that it implements `unlimited/autonomous`, that it is black-start capable, that it
    /// operates on phase B. A test case already marked mandatory is mandatory regardless.
    pub fn is_mandatory_for(&self, conditions: &[&str]) -> bool {
        self.level == Support::Mandatory
            || self
                .raised_when
                .iter()
                .any(|raise| conditions.contains(raise))
    }
}

/// How much of a set of abstract test cases a suite covers.
///
/// Built by [`Coverage::of`] from the test cases in scope and the identifiers a suite
/// exercises. Nothing here knows what a test *does* — the claim that a test covers an
/// abstract test case is the suite's, and is only as good as the test.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    covered: alloc::vec::Vec<&'static str>,
    missing: alloc::vec::Vec<&'static str>,
    unknown: alloc::vec::Vec<alloc::string::String>,
}

impl Coverage {
    /// Compares a suite's claims against the test cases in scope.
    ///
    /// Identifiers in `claimed` that name nothing in `scope` are reported separately by
    /// [`unknown`](Self::unknown) rather than silently counted: a typo in a test-case
    /// identifier would otherwise inflate the number it was meant to justify.
    pub fn of<'a>(scope: impl IntoIterator<Item = &'a AbstractTestCase>, claimed: &[&str]) -> Self {
        let mut report = Self::default();
        let mut matched = alloc::vec::Vec::new();
        for case in scope {
            if claimed.contains(&case.id) {
                report.covered.push(case.id);
                matched.push(case.id);
            } else {
                report.missing.push(case.id);
            }
        }
        for claim in claimed {
            if !matched.contains(claim) {
                report.unknown.push((*claim).into());
            }
        }
        report
    }

    /// How many of the test cases in scope the suite claims.
    pub fn covered(&self) -> usize {
        self.covered.len()
    }

    /// How many it does not.
    pub fn missing(&self) -> usize {
        self.missing.len()
    }

    /// The identifiers it does not cover, in catalogue order.
    pub fn missing_ids(&self) -> &[&'static str] {
        &self.missing
    }

    /// Claimed identifiers that name no test case in scope: typos, or a claim made
    /// against the wrong actor.
    pub fn unknown(&self) -> &[alloc::string::String] {
        &self.unknown
    }

    /// The share covered, from zero to one. An empty scope counts as fully covered.
    pub fn ratio(&self) -> f64 {
        let total = self.covered.len() + self.missing.len();
        if total == 0 {
            return 1.0;
        }
        self.covered.len() as f64 / total as f64
    }

    /// The share covered as a whole percentage, rounded down.
    ///
    /// Rounded down so that a suite one test short of complete cannot report 100%.
    pub fn percent(&self) -> u32 {
        (self.ratio() * 100.0) as u32
    }
}

/// The 51 abstract test cases of the LPC High-Level Test Specification 1.0.2.
static LPC: &[AbstractTestCase] = &[
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGHeartbeat_001",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-006"],
        description: "This test shall ensure that the EG sends its heartbeats regularly. The interval between 2 consecutive heartbeats shall not exceed 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGConnection_001",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-030"],
        description: "This test shall ensure that the EG sends its heartbeat followed by an APCL command after the EG has rebooted.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGConnection_002",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-030"],
        description: "This test shall ensure that the EG sends its heartbeat followed by an APCL command after restoring the connection to the CS.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGConnection_003",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the device is black-start capable"],
        requirements: &["LPC-TS-030"],
        description: "This test shall ensure that the EG automatically reconnects after a black start.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGMessages_001",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-001"],
        description: "This test shall ensure that the EG causes the CS to change its current state from unlimited/controlled to limited without a duration due to an external stimulus.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGMessages_002",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPC-TS-046"],
        description: "This test shall ensure that the EG resends its messages when receiving a NACK from the CS after the EG has rebooted.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGMessages_003",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPC-TS-001", "LPC-TS-001/2", "LPC-TS-002"],
        description: "This test shall ensure that the EG sends valid messages over an extended period of time. The tester (CS) is able to switch its internal states immediately according to received write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_EGMessages_004",
        use_case: names::LPC,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPC-TS-003", "LPC-TS-011/1", "LPC-TS-013/1"],
        description: "This test shall ensure that the EG sends valid messages over an extended period of time. The tester (CS) is able to switch its internal states immediately according to received write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSHeartbeat_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-007"],
        description: "This test shall ensure that the CS sends its heartbeats regularly. The interval between 2 consecutive heartbeats shall not exceed 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_NT_CSConnection_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-004", "LPC-TS-036"],
        description: "This test shall ensure that the CS does not evaluate APCL write commands until it first receives an EG heartbeat after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-003", "LPC-TS-036", "LPC-TS-037", "LPC-TS-038"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point (FCAPL) until it first receives an EG heartbeat and a following APCL write command after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_003",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-005", "LPC-TS-018", "LPC-TS-038"],
        description: "This test shall ensure that the CS only accepts APCL and FCAPL values greater or equal to zero.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_004",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-005", "LPC-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point (Failsafe Duration Minimum) until it first receives an EG heartbeat and a following APCL write command after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_005",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-014", "LPC-TS-015", "LPC-TS-015/1", "LPC-TS-016"],
        description: "This test shall ensure that the CS evaluates write commands to the Failsafe Duration Minimum if the submitted value is greater than the maximum value of the CS.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_006",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[],
        requirements: &["LPC-TS-035/4"],
        description: "This test shall ensure that the CS accepts APCL write commands with a larger value than the possible maximum consumption and alters the value accordingly.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_007",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-001", "LPC-TS-035", "LPC-TS-035/4"],
        description: "This test shall ensure that the CS correctly evaluates APCL write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_008",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-001", "LPC-TS-015/1", "LPC-TS-016", "LPC-TS-038"],
        description: "This test shall ensure that the CS correctly evaluates FCAPL and Failsafe Duration Minimum write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSConnection_009",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the device is black-start capable"],
        requirements: &["LPC-TS-046"],
        description: "This test shall ensure that the CS automatically reconnects after a black start.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSInit_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-009/3", "LPC-TS-011", "LPC-TS-017", "LPC-TS-019"],
        description: "This test shall ensure that the CS starts with a limited power consumption stated in the FCAPL and a deactivated APCL after a factory reset.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSInit_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-009/2", "LPC-TS-009/3", "LPC-TS-011", "LPC-TS-013"],
        description: "This test shall ensure that the CS starts with default parameters after a factory reset.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSInit_003",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPC-TS-011/1", "LPC-TS-013/1", "LPC-TS-044"],
        description: "This test shall ensure that the CS persistently stores the FCAPL and Failsafe Duration Minimum values.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_NT_CSLimited_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-009/1", "LPC-TS-024", "LPC-TS-035/1"],
        description: "This test shall ensure that the CS is limited with an activated APCL and maintains its state after rejecting invalid APCL commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSLimited_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-001/2", "LPC-TS-002"],
        description: "This test shall ensure that the CS maintains its state and accepts APCL write commands even if heartbeats from the EG are absent.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_NT_CSUnlCntrl_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-009", "LPC-TS-009/3", "LPC-TS-023"],
        description: "This test shall ensure that the CS maintains its state after rejecting invalid APCL commands.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSUnlCntrl_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "exactly one of CSUnlCntrl_002 (a CEM) and CSUnlCntrl_003 (not a CEM) is executed",
        ],
        requirements: &["LPC-TS-010/3", "LPC-TS-010/4", "LPC-TS-038", "LPC-TS-039"],
        description: "This test shall ensure that the CS supports and provides the Contractual Consumption Nominal Max value, as it is an actor of type CEM.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSUnlCntrl_003",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "exactly one of CSUnlCntrl_002 (a CEM) and CSUnlCntrl_003 (not a CEM) is executed",
        ],
        requirements: &["LPC-TS-010/1", "LPC-TS-010/2", "LPC-TS-038", "LPC-TS-040"],
        description: "This test shall ensure that the CS supports and provides the Power Consumption Nominal Max value, as it is not an actor of type CEM.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSFS_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-033", "LPC-TS-036", "LPC-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point until it first receives an EG heartbeat and a following APCL write command within 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSFS_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-012", "LPC-TS-013"],
        description: "This test shall ensure that the CS remains in failsafe state for the Failsafe Duration Minimum.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSFS_003",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-009", "LPC-TS-009/3"],
        description: "This test shall ensure that the CS rejects Failsafe Duration Minimum write commands in failsafe state.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_NT_CSUnlAuto_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-033", "LPC-TS-036", "LPC-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point until it first receives an EG heartbeat and a following APCL write command within 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSUnlAuto_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[],
        requirements: &["LPC-TS-009/3", "LPC-TS-010", "LPC-TS-038"],
        description: "This test shall ensure that the CS does not consume (or allow consumption) higher than the according nominal maximum value. The APCL is deactivated in CF_CS_UnlAuto.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition1_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-018", "LPC-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after rejecting an activated APCL with invalid value.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition1_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-021"],
        description: "This test shall ensure that the CS changes its state after accepting a deactivated APCL write command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition2_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-020"],
        description: "This test shall ensure that the CS changes its state after accepting an activated APCL command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition3_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-022", "LPC-TS-022/1"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat and a following APCL command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition3_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-022", "LPC-TS-022/1"],
        description: "This test shall ensure that the CS changes its state after receiving a heartbeat, but no following APCL write command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition4_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-027"],
        description: "This test shall ensure that the CS changes its state after receiving and accepting an APCL command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition5_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-028"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition6_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-001/1", "LPC-TS-008", "LPC-TS-008/1", "LPC-TS-025"],
        description: "This test shall ensure that the CS changes its state after the APCL duration is expired.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition6_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-026"],
        description: "This test shall ensure that the CS changes its state after receiving an APCL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition7_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-029"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition8_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-031", "LPC-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after receiving an APCL command which cannot be applied.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition8_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-033"],
        description: "This test shall ensure that the CS changes its state after receiving an APCL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition9_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPC-TS-032"],
        description: "This test shall ensure that the CS changes its state after receiving an APCL activation command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition10_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-012", "LPC-TS-022", "LPC-TS-022/3"],
        description: "This test shall ensure that the CS changes its state after expiry of the Failsafe Duration Minimum.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition10_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-022", "LPC-TS-022/2"],
        description: "This test shall ensure that the CS changes its state after not receiving an APCL command within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition11_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-031", "LPC-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after declining an APCL command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition11_002",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-033"],
        description: "This test shall ensure that the CS changes its state after receiving an APCL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_COM_PT_CSTransition12_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPC-TS-032"],
        description: "This test shall ensure that the CS changes its state after receiving a heartbeat and a following APCL activation command.",
    },
    AbstractTestCase {
        id: "ATC_LPC_INS1_PT_CSTransition1_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the manufacturer documents how the rejection can be provoked"],
        requirements: &["LPC-TS-035", "LPC-TS-035/2"],
        description: "This test shall ensure that the CS receives and accepts the initial APCL deactivation write command and rejects the following APCL write command due to exceptions permitted by [LPC1.0.0].",
    },
    AbstractTestCase {
        id: "ATC_LPC_INS2_PT_CSTransition1_001",
        use_case: names::LPC,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the manufacturer documents how the rejection can be provoked"],
        requirements: &["LPC-TS-035", "LPC-TS-035/3"],
        description: "This test shall ensure that the CS receives and accepts the initial APCL deactivation write command and rejects the following APCL write command due to exceptions permitted by [LPC1.0.0].",
    },
];

/// The 51 abstract test cases of the LPP High-Level Test Specification 1.0.2.
static LPP: &[AbstractTestCase] = &[
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGHeartbeat_001",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-006"],
        description: "This test shall ensure that the EG sends its heartbeats regularly. The interval between 2 consecutive heartbeats shall not exceed 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGConnection_001",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-030"],
        description: "This test shall ensure that the EG sends its heartbeat followed by an APPL command after the EG has rebooted.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGConnection_002",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-030"],
        description: "This test shall ensure that the EG sends its heartbeat followed by an APPL command after restoring the connection to the CS.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGConnection_003",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the device is black-start capable"],
        requirements: &["LPP-TS-030"],
        description: "This test shall ensure that the EG automatically reconnects after a black start.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGMessages_001",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-001"],
        description: "This test shall ensure that the EG causes the CS to change its current state from unlimited/controlled to limited without a duration due to an external stimulus.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGMessages_002",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPP-TS-046"],
        description: "This test shall ensure that the EG resends its messages when receiving a NACK from the CS after the EG has rebooted.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGMessages_003",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPP-TS-001", "LPP-TS-001/2", "LPP-TS-002"],
        description: "This test shall ensure that the EG sends valid messages over an extended period of time. The tester (CS) is able to switch its internal states immediately according to received write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_EGMessages_004",
        use_case: names::LPP,
        dut: actors::ENERGY_GUARD,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPP-TS-003", "LPP-TS-011/1", "LPP-TS-013/1"],
        description: "This test shall ensure that the EG sends valid messages over an extended period of time. The tester (CS) is able to switch its internal states immediately according to received write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSHeartbeat_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-007"],
        description: "This test shall ensure that the CS sends its heartbeats regularly. The interval between 2 consecutive heartbeats shall not exceed 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_NT_CSConnection_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-004", "LPP-TS-036"],
        description: "This test shall ensure that the CS does not evaluate APPL write commands until it first receives an EG heartbeat after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-003", "LPP-TS-036", "LPP-TS-037", "LPP-TS-038"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point (FPAPL) until it first receives an EG heartbeat and a following APPL write command after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_003",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-005", "LPP-TS-018", "LPP-TS-038"],
        description: "This test shall ensure that the CS only accepts APPL values smaller than or equal to zero and FPAPL values greater than or equal to zero.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_004",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-005", "LPP-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point (Failsafe Duration Minimum) until it first receives an EG heartbeat and a following APPL write command after the connection is established.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_005",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-014", "LPP-TS-015", "LPP-TS-015/1", "LPP-TS-016"],
        description: "This test shall ensure that the CS evaluates write commands to the Failsafe Duration Minimum if the submitted value is greater than the maximum value of the CS.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_006",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[],
        requirements: &["LPP-TS-035/4"],
        description: "This test shall ensure that the CS accepts APPL write commands with a larger value than the possible maximum production and alters the value accordingly.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_007",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-001", "LPP-TS-035", "LPP-TS-035/4"],
        description: "This test shall ensure that the CS correctly evaluates APPL write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_008",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-001", "LPP-TS-015/1", "LPP-TS-016", "LPP-TS-038"],
        description: "This test shall ensure that the CS correctly evaluates FPAPL and Failsafe Duration Minimum write commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSConnection_009",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the device is black-start capable"],
        requirements: &["LPP-TS-046"],
        description: "This test shall ensure that the CS automatically reconnects after a black start.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSInit_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-009/3", "LPP-TS-011", "LPP-TS-017", "LPP-TS-019"],
        description: "This test shall ensure that the CS starts with a limited power production stated in the FPAPL and a deactivated APPL after a factory reset.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSInit_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-009/2", "LPP-TS-009/3", "LPP-TS-011", "LPP-TS-013"],
        description: "This test shall ensure that the CS starts with default parameters after a factory reset.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSInit_003",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[],
        requirements: &["LPP-TS-011/1", "LPP-TS-013/1", "LPP-TS-044"],
        description: "This test shall ensure that the CS persistently stores the FPAPL and Failsafe Duration Minimum values.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_NT_CSLimited_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-009/1", "LPP-TS-024", "LPP-TS-035/1"],
        description: "This test shall ensure that the CS is limited with an activated APPL and maintains its state after rejecting invalid APPL commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSLimited_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-001/2", "LPP-TS-002"],
        description: "This test shall ensure that the CS maintains its state and accepts APPL write commands even if heartbeats from the EG are absent.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_NT_CSUnlCntrl_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-009", "LPP-TS-009/3", "LPP-TS-023"],
        description: "This test shall ensure that the CS maintains its state after rejecting invalid APPL commands.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSUnlCntrl_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "exactly one of CSUnlCntrl_002 (a CEM) and CSUnlCntrl_003 (not a CEM) is executed",
        ],
        requirements: &["LPP-TS-010/3", "LPP-TS-010/4", "LPP-TS-038", "LPP-TS-039"],
        description: "This test shall ensure that the CS supports and provides the Contractual Production Nominal Max value, as it is an actor of type CEM.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSUnlCntrl_003",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "exactly one of CSUnlCntrl_002 (a CEM) and CSUnlCntrl_003 (not a CEM) is executed",
        ],
        requirements: &["LPP-TS-010/1", "LPP-TS-010/2", "LPP-TS-038", "LPP-TS-040"],
        description: "This test shall ensure that the CS supports and provides the Power Production Nominal Max value, as it is not an actor of type CEM.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSFS_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-033", "LPP-TS-036", "LPP-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point until it first receives an EG heartbeat and a following APPL write command within 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSFS_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-012", "LPP-TS-013"],
        description: "This test shall ensure that the CS remains in failsafe state for the Failsafe Duration Minimum.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSFS_003",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-009", "LPP-TS-009/3"],
        description: "This test shall ensure that the CS rejects Failsafe Duration Minimum write commands in failsafe state.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_NT_CSUnlAuto_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-033", "LPP-TS-036", "LPP-TS-037"],
        description: "This test shall ensure that the CS does not evaluate write commands to any data point until it first receives an EG heartbeat and a following APPL write command within 60 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSUnlAuto_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[],
        requirements: &["LPP-TS-009/3", "LPP-TS-010", "LPP-TS-038"],
        description: "This test shall ensure that the CS does not produce (or allow production) higher than the according nominal maximum value. The APPL is deactivated in CF_CS_UnlAuto.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition1_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-018", "LPP-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after rejecting an activated APPL with invalid value.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition1_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-021"],
        description: "This test shall ensure that the CS changes its state after accepting a deactivated APPL write command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition2_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-020"],
        description: "This test shall ensure that the CS changes its state after accepting an activated APPL command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition3_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-022", "LPP-TS-022/1"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat and a following APPL command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition3_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-022", "LPP-TS-022/1"],
        description: "This test shall ensure that the CS changes its state after receiving a heartbeat, but no following APPL write command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition4_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-027"],
        description: "This test shall ensure that the CS changes its state after receiving and accepting an APPL command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition5_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-028"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition6_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-001/1", "LPP-TS-008", "LPP-TS-008/1", "LPP-TS-025"],
        description: "This test shall ensure that the CS changes its state after the APPL duration is expired.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition6_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-026"],
        description: "This test shall ensure that the CS changes its state after receiving an APPL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition7_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-029"],
        description: "This test shall ensure that the CS changes its state after not receiving a heartbeat within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition8_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-031", "LPP-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after receiving an APPL command which cannot be applied.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition8_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-033"],
        description: "This test shall ensure that the CS changes its state after receiving an APPL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition9_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["LPP-TS-032"],
        description: "This test shall ensure that the CS changes its state after receiving an APPL activation command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition10_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-012", "LPP-TS-022", "LPP-TS-022/3"],
        description: "This test shall ensure that the CS changes its state after expiry of the Failsafe Duration Minimum.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition10_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-022", "LPP-TS-022/2"],
        description: "This test shall ensure that the CS changes its state after not receiving an APPL command within 120 seconds.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition11_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-031", "LPP-TS-035/1"],
        description: "This test shall ensure that the CS changes its state after declining an APPL command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition11_002",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-033"],
        description: "This test shall ensure that the CS changes its state after receiving an APPL deactivation command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_COM_PT_CSTransition12_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the \"unlimited/autonomous\" state is implemented"],
        requirements: &["LPP-TS-032"],
        description: "This test shall ensure that the CS changes its state after receiving a heartbeat and a following APPL activation command.",
    },
    AbstractTestCase {
        id: "ATC_LPP_INS1_PT_CSTransition1_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the manufacturer documents how the rejection can be provoked"],
        requirements: &["LPP-TS-035", "LPP-TS-035/2"],
        description: "This test shall ensure that the CS receives and accepts the initial APPL deactivation write command and rejects the following APPL write command due to exceptions permitted by [LPP1.0.0].",
    },
    AbstractTestCase {
        id: "ATC_LPP_INS2_PT_CSTransition1_001",
        use_case: names::LPP,
        dut: actors::CONTROLLABLE_SYSTEM,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the manufacturer documents how the rejection can be provoked"],
        requirements: &["LPP-TS-035", "LPP-TS-035/3"],
        description: "This test shall ensure that the CS receives and accepts the initial APPL write command and rejects the following APPL write command due to exceptions permitted by [LPP1.0.0].",
    },
];

/// The 54 abstract test cases of the MPC High-Level Test Specification 1.0.2.
static MPC: &[AbstractTestCase] = &[
    AbstractTestCase {
        id: "ATC_MPC_COM_PT_MUPolling_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MPC-TS-013"],
        description: "This test shall ensure that the MU sends its data after receiving requests of the MA. The interval between 2 consecutive requests shall not exceed 120 seconds. The tester must choose and document a data point from the manufacturer's specification in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MPC_COM_PT_MUNotification_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MPC-TS-014"],
        description: "This test shall ensure that the MU sends its data regularly and reliably after a value has changed. After the value change, the changed data shall be transmitted within 120 seconds. *1 The tester must choose a data point from the manufacturer's specification in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MUTotalActivePower_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MPC-TS-001", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the momentary power consumption/production.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MUPhaseActivePower_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/1", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active power on phase A while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MUPhaseActivePower_002",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/2", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active power on phase B while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MUPhaseActivePower_003",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/3", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active power on phase C while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MUTotalConsumedEnergy_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-003", "MPC-TS-003/1", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends a positive total consumed energy while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MUTotalConsumedEnergy_002",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-003", "MPC-TS-003/1", "MPC-TS-009", "MPC-TS-013"],
        description: "This test shall ensure that the MU sends a positive total consumed energy while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MUTotalProducedEnergy_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-004", "MPC-TS-004/1", "MPC-TS-009", "MPC-TS-013"],
        description: "This test shall ensure that the MU sends a negative total produced energy while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MUTotalProducedEnergy_002",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-004", "MPC-TS-004/1", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends a negative total produced energy while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MUActiveACCurrent_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/1", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active AC current on phase A while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MUActiveACCurrent_002",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/2", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active AC current on phase B while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MUActiveACCurrent_003",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/3", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MU sends the phase-specific active AC current on phase C while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/1", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase A and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_002",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/2", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase B and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_003",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/3", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase C and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_004",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/4", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase A and phase B.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_005",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/5", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase B and phase C.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MUACVoltage_006",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/6", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MU sends the phase-specific AC voltage between phase C and phase A.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE5_PT_MUFrequency_001",
        use_case: names::MPC,
        dut: actors::MONITORED_UNIT,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MPC-TS-007"],
        description: "This test shall ensure that the MU sends the frequency.",
    },
    AbstractTestCase {
        id: "ATC_MPC_COM_PT_MAPolling_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MPC-TS-013"],
        description: "This test shall ensure that the MA requests data from the MU regularly. The interval between 2 consecutive requests must be specified in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MPC_COM_PT_MANotification_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MPC-TS-014"],
        description: "This test shall ensure that the MA receives a notification after a value has changed. After the value change, the changed data shall be transmitted within 120 seconds. *1",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MATotalActivePower_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MPC-TS-001", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_NT_MATotalActivePower_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Negative,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MAPhaseActivePower_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/1", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase A with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_NT_MAPhaseActivePower_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase A with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MAPhaseActivePower_003",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/2", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase B with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_NT_MAPhaseActivePower_004",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase B with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_PT_MAPhaseActivePower_005",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-002", "MPC-TS-002/3", "MPC-TS-002/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase C with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE1_NT_MAPhaseActivePower_006",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production on phase C with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MATotalConsumedEnergy_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-003", "MPC-TS-003/1", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the total consumed energy with value state \"normal\" while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_NT_MATotalConsumedEnergy_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the total consumed energy with value state \"error\" or \"out of range\" while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_PT_MATotalProducedEnergy_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-004", "MPC-TS-004/1", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the total produced energy with value state \"normal\" while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE2_NT_MATotalProducedEnergy_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the data point is supported and the device can consume or produce energy"],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the total produced energy with value state \"error\" or \"out of range\" while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MAActiveACCurrent_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/1", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase A with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_NT_MAActiveACCurrent_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Negative,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase A with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MAActiveACCurrent_003",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/2", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase B with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_NT_MAActiveACCurrent_004",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Negative,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase B with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_PT_MAActiveACCurrent_005",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-005", "MPC-TS-005/3", "MPC-TS-005/4", "MPC-TS-009"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase C with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE3_NT_MAActiveACCurrent_006",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Negative,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase C with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/1", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_003",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/2", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_004",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_005",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/3", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_006",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_007",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/4", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and phase B with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_008",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and phase B with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_009",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/5", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and phase C with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_010",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and phase C with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_PT_MAACVoltage_011",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-006", "MPC-TS-006/6", "MPC-TS-006/7", "MPC-TS-010"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and phase A with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE4_NT_MAACVoltage_012",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and phase A with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE5_PT_MAFrequency_001",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MPC-TS-007"],
        description: "This test shall ensure that the MA receives the frequency with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MPC_SCE5_NT_MAFrequency_002",
        use_case: names::MPC,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MPC-TS-008", "MPC-TS-008/1", "MPC-TS-008/2"],
        description: "This test shall ensure that the MA receives the frequency with value state \"error\" or \"out of range\".",
    },
];

/// The 47 abstract test cases of the MGCP High-Level Test Specification 1.0.2.
static MGCP: &[AbstractTestCase] = &[
    AbstractTestCase {
        id: "ATC_MGCP_COM_PT_GCPPolling_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MGCP-TS-012"],
        description: "This test shall ensure that the GCP sends its data after receiving requests of the MA. The interval between 2 consecutive requests shall not exceed 120 seconds. The tester must choose and document a data point from the manufacturer's specification in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MGCP_COM_PT_GCPNotification_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MGCP-TS-013"],
        description: "This test shall ensure that the GCP sends its data regularly and reliably after a value has changed. After the value change, the changed data shall be transmitted within 120 seconds. *1 The tester must choose a data point from the manufacturer's specification in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE1_PT_GCPPowerLimitFactor_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MGCP-TS-001"],
        description: "This test shall ensure that the GCP sends the PV feed-in power limitation factor.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE2_PT_GCPTotalActivePower_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MGCP-TS-002", "MGCP-TS-010"],
        description: "This test shall ensure that the GCP sends the momentary power consumption/production.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE3_PT_GCPTotalFeedInEnergy_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MGCP-TS-003", "MGCP-TS-010", "MGCP-TS-012"],
        description: "This test shall ensure that the GCP sends a negative total feed-in energy while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE3_PT_GCPTotalFeedInEnergy_002",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MGCP-TS-003", "MGCP-TS-010"],
        description: "This test shall ensure that the GCP sends a negative total feed-in energy while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE4_PT_GCPTotalConsumedEnergy_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MGCP-TS-004", "MGCP-TS-010"],
        description: "This test shall ensure that the GCP sends a positive total consumed energy while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE4_PT_GCPTotalConsumedEnergy_002",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Mandatory,
        raised_when: &[],
        requirements: &["MGCP-TS-004", "MGCP-TS-010", "MGCP-TS-012"],
        description: "This test shall ensure that the GCP sends a positive total consumed energy while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/1",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific active AC current on phase A while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_002",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/2",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific active AC current on phase B while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_003",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/3",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific active AC current on phase C while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/1",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase A and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_002",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/2",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase B and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_003",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/3",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase C and neutral.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_004",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/4",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase A and phase B.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_005",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/5",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase B and phase C.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_GCPACVoltage_006",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/6",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the GCP sends the phase-specific AC voltage between phase C and phase A.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE7_PT_GCPFrequency_001",
        use_case: names::MGCP,
        dut: actors::GRID_CONNECTION_POINT,
        scenario: Some(7),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MGCP-TS-007"],
        description: "This test shall ensure that the GCP sends the frequency.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_COM_PT_MAPolling_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MGCP-TS-012"],
        description: "This test shall ensure that the MA requests data from the GCP regularly. The interval between 2 consecutive requests must be specified in the [ParameterSheet].",
    },
    AbstractTestCase {
        id: "ATC_MGCP_COM_PT_MANotification_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: None,
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the appliance polls or notifies"],
        requirements: &["MGCP-TS-013"],
        description: "This test shall ensure that the MA receives a notification after a value has changed. After the value change, the changed data shall be transmitted within 120 seconds. *1",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE1_PT_MAPowerLimitFactor_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(1),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MGCP-TS-001"],
        description: "This test shall ensure that the MA receives the PV feed-in power limitation factor.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE2_PT_MATotalActivePower_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Positive,
        level: Support::Recommended,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-002", "MGCP-TS-010"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE2_NT_MATotalActivePower_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(2),
        kind: Kind::Negative,
        level: Support::Recommended,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the momentary power consumption/production with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE3_PT_MATotalFeedInEnergy_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-003", "MGCP-TS-010"],
        description: "This test shall ensure that the MA receives the total feed-in energy with value state \"normal\" while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE3_NT_MATotalFeedInEnergy_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(3),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the total feed-in energy with value state \"error\" or \"out of range\" while producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE4_PT_MATotalConsumedEnergy_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-004", "MGCP-TS-010"],
        description: "This test shall ensure that the MA receives the total consumed energy with value state \"normal\" while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE4_NT_MATotalConsumedEnergy_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(4),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the MA supports this data point; it must support at least one of them"],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the total consumed energy with value state \"error\" or \"out of range\" while consuming energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_MAActiveACCurrent_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/1",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase A with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_NT_MAActiveACCurrent_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase A with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_MAActiveACCurrent_003",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/2",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase B with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_NT_MAActiveACCurrent_004",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase B with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_PT_MAActiveACCurrent_005",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-005",
            "MGCP-TS-005/3",
            "MGCP-TS-005/4",
            "MGCP-TS-010",
        ],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase C with value state \"normal\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE5_NT_MAActiveACCurrent_006",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(5),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific active AC current on phase C with value state \"error\" or \"out of range\" while consuming/producing energy.",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/1",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_003",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/2",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_004",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_005",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/3",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and neutral with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_006",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and neutral with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_007",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/4",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and phase B with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_008",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase A and phase B with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_009",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/5",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and phase C with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_010",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase B and phase C with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_PT_MAACVoltage_011",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &[
            "MGCP-TS-006",
            "MGCP-TS-006/6",
            "MGCP-TS-006/7",
            "MGCP-TS-011",
        ],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and phase A with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE6_NT_MAACVoltage_012",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(6),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &[
            "the phase is one the device actually operates on",
            "the manufacturer documents how the value can be driven",
        ],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the phase-specific AC voltage between phase C and phase A with value state \"error\" or \"out of range\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE7_PT_MAFrequency_001",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(7),
        kind: Kind::Positive,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MGCP-TS-007"],
        description: "This test shall ensure that the MA receives the frequency with value state \"normal\".",
    },
    AbstractTestCase {
        id: "ATC_MGCP_SCE7_NT_MAFrequency_002",
        use_case: names::MGCP,
        dut: actors::MONITORING_APPLIANCE,
        scenario: Some(7),
        kind: Kind::Negative,
        level: Support::Optional,
        raised_when: &["the data point is supported"],
        requirements: &["MGCP-TS-008", "MGCP-TS-008/1", "MGCP-TS-008/2"],
        description: "This test shall ensure that the MA receives the frequency with value state \"error\" or \"out of range\".",
    },
];
/// Every abstract test case of the four certifiable use cases: LPC, LPP, MPC and MGCP.
///
/// Transcribed from the 1.0.2 High-Level Test Specifications, in the order their coverage
/// tables list them.
pub static CATALOGUE: &[&[AbstractTestCase]] = &[LPC, LPP, MPC, MGCP];

/// Why a test case is the device's to answer rather than the library's.
///
/// Five reasons cover the fourteen cases; they are constants so that the same thing is
/// always said the same way, in a report and in a parameter sheet.
pub mod device {
    /// A factory reset and the defaults it restores are the device's, not the library's.
    pub const FACTORY_RESET: &str =
        "a factory reset and the defaults it restores are the device's, not the library's";
    /// The library holds the values in memory; where they survive a power cut is the
    /// device's decision.
    pub const PERSISTENCE: &str = "the library holds the values in memory and hands them to the application; where \
         they are stored across a power cut is the device's decision";
    /// Powering the device off and on again.
    pub const BLACK_START: &str = "powering the device off and on again; the runtime handles the dialling back, \
         but the power supply is not something a library can cut";
    /// A start-up duration is the device's, not a rebuilt actor's.
    pub const REBOOT: &str = "the start-up duration measured is the device's, not that of an actor a test \
         rebuilds in memory";
    /// What the appliance actually draws.
    pub const APPLIANCE: &str = "what the appliance actually draws, which the library reports a ceiling for and \
         does not control";
}

/// The test cases no library can answer, with the reason each is the device's.
///
/// Roughly a third of the LPC and LPP catalogues are about the device rather than the
/// protocol — a factory reset, a power cut, a start-up duration, what the appliance
/// actually draws. `cargo test` here does not count them as covered, and a consumer's
/// harness driving a real device is where they are answered: this table is that
/// harness's checklist, one row per `ATC_…` identifier.
///
/// The seven are the same for LPC and LPP, so there are fourteen rows.
pub static DEVICE_LEVEL: &[(&str, &str)] = &[
    ("ATC_LPC_COM_PT_CSConnection_006", device::APPLIANCE),
    ("ATC_LPC_COM_PT_CSConnection_009", device::BLACK_START),
    ("ATC_LPC_COM_PT_CSInit_002", device::FACTORY_RESET),
    ("ATC_LPC_COM_PT_CSInit_003", device::PERSISTENCE),
    ("ATC_LPC_COM_PT_EGConnection_001", device::REBOOT),
    ("ATC_LPC_COM_PT_EGConnection_003", device::BLACK_START),
    ("ATC_LPC_COM_PT_EGMessages_002", device::REBOOT),
    ("ATC_LPP_COM_PT_CSConnection_006", device::APPLIANCE),
    ("ATC_LPP_COM_PT_CSConnection_009", device::BLACK_START),
    ("ATC_LPP_COM_PT_CSInit_002", device::FACTORY_RESET),
    ("ATC_LPP_COM_PT_CSInit_003", device::PERSISTENCE),
    ("ATC_LPP_COM_PT_EGConnection_001", device::REBOOT),
    ("ATC_LPP_COM_PT_EGConnection_003", device::BLACK_START),
    ("ATC_LPP_COM_PT_EGMessages_002", device::REBOOT),
];

impl AbstractTestCase {
    /// Why this test case is the device's to answer, if it is.
    ///
    /// [`None`] for a test case a library can drive — which is every one this crate's
    /// own suite drives. [`Some`] carries the reason, which is what the device's harness
    /// has to show the laboratory.
    ///
    /// ```
    /// use eebus::conformance;
    ///
    /// let reset = conformance::find("ATC_LPC_COM_PT_CSInit_002").unwrap();
    /// assert!(reset.owed_by_device().is_some(), "a factory reset is the device's");
    ///
    /// let timer = conformance::find("ATC_LPC_COM_PT_CSTransition5_001").unwrap();
    /// assert!(timer.owed_by_device().is_none(), "a heartbeat timeout is the library's");
    /// ```
    pub fn owed_by_device(&self) -> Option<&'static str> {
        DEVICE_LEVEL
            .iter()
            .find(|(id, _)| *id == self.id)
            .map(|(_, reason)| *reason)
    }
}

/// Every test case the device has to answer for itself, with the reason.
///
/// The checklist for a harness that drives a real device: iterate it, and for each row
/// arrange the condition the description names — cut the power, trigger the reset — and
/// check the result the description asks for. Filter by [`AbstractTestCase::dut`] for
/// one actor, or by [`AbstractTestCase::is_mandatory_for`] for what the parameter sheet
/// commits to.
///
/// ```
/// use eebus::conformance;
/// use eebus::usecases::descriptor::actors;
///
/// let mine: Vec<_> = conformance::device_level()
///     .filter(|(case, _)| case.dut == actors::CONTROLLABLE_SYSTEM)
///     .collect();
/// assert_eq!(mine.len(), 8, "four for LPC and four for LPP");
/// ```
pub fn device_level() -> impl Iterator<Item = (&'static AbstractTestCase, &'static str)> {
    DEVICE_LEVEL
        .iter()
        .filter_map(|(id, reason)| find(id).map(|case| (case, *reason)))
}

/// Looks a test case up by its `ATC_…` identifier.
pub fn find(id: &str) -> Option<&'static AbstractTestCase> {
    CATALOGUE
        .iter()
        .flat_map(|cases| cases.iter())
        .find(|case| case.id == id)
}

/// Every test case of one use case, named as
/// [`descriptor::names`](crate::usecases::descriptor::names) spells it.
pub fn for_use_case(use_case: &str) -> impl Iterator<Item = &'static AbstractTestCase> {
    CATALOGUE
        .iter()
        .flat_map(|cases| cases.iter())
        .filter(move |case| case.use_case == use_case)
}

/// Every test case that puts one actor of one use case under test.
///
/// This is the set a device implementing that actor is measured against — the other
/// actor's test cases are the tester's job, not the device's.
pub fn for_actor(
    use_case: &'static str,
    actor: &'static str,
) -> impl Iterator<Item = &'static AbstractTestCase> {
    for_use_case(use_case).filter(move |case| case.dut == actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The four specifications between them define 203 abstract test cases, which is the
    /// "over 200 UC-level tests" the tester is usually described as running.
    #[test]
    fn the_catalogue_holds_every_test_case_of_the_four_specifications() {
        let counts: Vec<usize> = CATALOGUE.iter().map(|cases| cases.len()).collect();
        assert_eq!(counts, [51, 51, 54, 47], "LPC, LPP, MPC, MGCP");
        assert_eq!(counts.iter().sum::<usize>(), 203);
    }

    /// Every device-level row names a real test case, and the table is the same seven
    /// for LPC and LPP: a row that exists for one and not the other is a transcription
    /// error, since the two specifications are structurally identical.
    #[test]
    fn the_device_level_table_names_real_cases_the_same_way_for_both_use_cases() {
        assert_eq!(DEVICE_LEVEL.len(), 14);
        assert_eq!(device_level().count(), 14, "every row resolves");
        let lpc: Vec<&str> = DEVICE_LEVEL
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("ATC_LPC_"))
            .collect();
        let lpp: Vec<&str> = DEVICE_LEVEL
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("ATC_LPP_"))
            .collect();
        assert_eq!(lpc, lpp, "the same seven under each prefix");
        assert_eq!(lpc.len(), 7);
        for (case, reason) in device_level() {
            assert_eq!(case.owed_by_device(), Some(reason));
            assert!(
                case.use_case == names::LPC || case.use_case == names::LPP,
                "{}: the measurement use cases have no device-level cases",
                case.id
            );
        }
    }

    #[test]
    fn every_identifier_is_unique_and_well_formed() {
        let mut seen: Vec<&str> = Vec::new();
        for case in CATALOGUE.iter().flat_map(|cases| cases.iter()) {
            assert!(case.id.starts_with("ATC_"), "{}", case.id);
            assert!(!seen.contains(&case.id), "duplicate {}", case.id);
            assert!(
                !case.requirements.is_empty(),
                "{} covers no requirement",
                case.id
            );
            assert!(
                case.description.starts_with("This test shall ensure"),
                "{}",
                case.id
            );
            seen.push(case.id);
        }
    }

    /// The identifier, the use case and the actor have to agree: `ATC_LPC_…_CS…` is a
    /// Controllable System test case of LPC and nothing else.
    #[test]
    fn the_identifier_agrees_with_the_fields() {
        for case in CATALOGUE.iter().flat_map(|cases| cases.iter()) {
            let expected = match case.abbreviation() {
                "LPC" => names::LPC,
                "LPP" => names::LPP,
                "MPC" => names::MPC,
                "MGCP" => names::MGCP,
                other => panic!("unknown use case {other} in {}", case.id),
            };
            assert_eq!(case.use_case, expected, "{}", case.id);

            let name = case.id.rsplit('_').nth(1).expect("a test-case name");
            let expected_dut = match &name[..2] {
                "EG" => actors::ENERGY_GUARD,
                "CS" => actors::CONTROLLABLE_SYSTEM,
                "MU" => actors::MONITORED_UNIT,
                "MA" => actors::MONITORING_APPLIANCE,
                "GC" => actors::GRID_CONNECTION_POINT,
                other => panic!("unknown actor prefix {other} in {}", case.id),
            };
            assert_eq!(case.dut, expected_dut, "{}", case.id);
        }
    }

    /// An `o/m` marking is meaningless without the condition that raises it.
    #[test]
    fn an_optional_test_case_that_can_become_mandatory_says_when() {
        let conditional: Vec<&str> = CATALOGUE
            .iter()
            .flat_map(|cases| cases.iter())
            .filter(|case| !case.raised_when.is_empty())
            .map(|case| case.id)
            .collect();
        assert!(!conditional.is_empty());

        let autonomous = find("ATC_LPC_COM_PT_CSTransition10_001").expect("in the catalogue");
        assert_eq!(autonomous.level, Support::Optional);
        assert!(!autonomous.is_mandatory_for(&[]));
        assert!(
            autonomous.is_mandatory_for(&[r#"the "unlimited/autonomous" state is implemented"#]),
            "this crate implements the state, so the laboratory will run it"
        );
    }

    /// The counts a device planning a certification slot actually budgets against.
    #[test]
    fn the_actor_filters_split_each_specification_in_two() {
        let cs = for_actor(names::LPC, actors::CONTROLLABLE_SYSTEM).count();
        let eg = for_actor(names::LPC, actors::ENERGY_GUARD).count();
        assert_eq!(cs + eg, 51);
        assert_eq!(eg, 8, "LPC Table 13 marks eight for the Energy Guard");

        let gcp = for_actor(names::MGCP, actors::GRID_CONNECTION_POINT).count();
        let ma = for_actor(names::MGCP, actors::MONITORING_APPLIANCE).count();
        assert_eq!(gcp + ma, 47);
    }

    #[test]
    fn a_claim_that_names_nothing_is_reported_rather_than_counted() {
        let scope: Vec<_> = for_actor(names::LPC, actors::CONTROLLABLE_SYSTEM).collect();
        let report = Coverage::of(
            scope.iter().copied(),
            &["ATC_LPC_COM_PT_CSTransition5_001", "ATC_LPC_TYPO_001"],
        );
        assert_eq!(report.covered(), 1);
        assert_eq!(report.unknown(), ["ATC_LPC_TYPO_001"]);
        assert_eq!(report.percent(), 2);
    }

    /// One short of complete must not read as complete.
    #[test]
    fn the_percentage_rounds_down() {
        let scope: Vec<_> = for_actor(names::MGCP, actors::MONITORING_APPLIANCE).collect();
        let claimed: Vec<&str> = scope.iter().skip(1).map(|case| case.id).collect();
        let report = Coverage::of(scope.iter().copied(), &claimed);
        assert_eq!(report.missing(), 1);
        assert_eq!(
            report.percent(),
            96,
            "27 of 28 is not 97%, and is certainly not 100%"
        );
    }
}
