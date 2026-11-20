//! The SHIP, SPINE and SHIP Pairing Service test specifications as data.
//!
//! A laboratory certifying a device does not stop at the use cases. Below them it runs
//! three more case lists: the SHIP Test Specification 1.0.0, the SPINE Test Specification
//! 1.0.0 and the SHIP Pairing Service TestSpec 1.0.0. This module carries every case of
//! the three — identifier, heading, the DUT column of the specification's mapping table,
//! the requirement identifiers it verifies, and the parameter-sheet conditions under which
//! it applies — so that the gap is a number rather than a reading.
//!
//! [`Specification::cases`] is each list; [`Specification::coverage`] measures a suite's
//! claims against it with the same [`Coverage`] the use-case catalogue uses.
//! `tests/conformance.rs` in this crate does exactly that: a case counts as covered when a
//! `#[test]` names it in its doc comment, and the uncovered cases of each specification are
//! asserted by name.
//!
//! # Applicability, not optionality
//!
//! The SHIP and SPINE mapping tables mark every case mandatory, and the Pairing Service
//! specification has no mandatory/optional column at all. What a device's declaration
//! changes is whether a case *applies*: a SHIP case that needs a SHIP server announced over
//! mDNS applies only where `PAR_shipSvc` is `"yes"`, a SPINE case about the Energy Guard only
//! to a device whose `DUT Actor` includes `EG`. Each case carries those conditions in
//! [`TestCase::applies_when`], and a [`ParameterSheet`] — the declared `(parameter, value)`
//! pairs from the specification's own parameter sheet — selects the cases that are
//! mandatory for one device:
//!
//! ```
//! use eebus::conformance::protocol::{ParameterSheet, Specification};
//!
//! // A Controllable System that never initiates detailed discovery.
//! let sheet = ParameterSheet::new()
//!     .declare("DUT Actor", "CS")
//!     .declare("PAR_clientDetailedDiscoverySupported", "no")
//!     .declare("PAR_clientSubscriptionSupported", "yes")
//!     .declare("PAR_initialTimeoutSupported", "yes");
//! assert!(sheet.unknown(Specification::Spine).is_empty(), "every name is on the sheet");
//!
//! let mandatory: Vec<_> = Specification::Spine.mandatory(&sheet).map(|tc| tc.id).collect();
//! assert!(mandatory.contains(&"TC_SPINE_BIND_002"), "a CS case");
//! assert!(!mandatory.contains(&"TC_SPINE_RTC_003"), "an EG case");
//! assert!(!mandatory.contains(&"TC_SPINE_DDISC_001"), "needs client detailed discovery");
//! ```
//!
//! # What is carried, and what is not
//!
//! The specifications are free after registration but not redistributable. What is
//! carried is what a test report names: the identifier, the case's heading, the DUT
//! column, the requirement identifiers and the applicability. The description paragraphs
//! and the test steps stay in the specifications.
//!
//! One case is the device's rather than the library's: `TC_SPS_DIGEST_003` power-cycles
//! the device and checks that the Pairing Service's replay ring buffer survived.
//! [`DEVICE_LEVEL`] carries it with the reason and a procedure for a consumer to drive
//! against its own binary, as [`harness`](super::harness) does for the use-case cases.

use alloc::string::String;
use alloc::vec::Vec;

use super::Coverage;
use super::device;
use super::harness::Step;

use Condition::{AnyOf, AtLeast, Is};

/// One of the three protocol-level test specifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Specification {
    /// The SHIP Test Specification 1.0.0.
    Ship,
    /// The SPINE Test Specification 1.0.0.
    Spine,
    /// The SHIP Pairing Service TestSpec 1.0.0.
    PairingService,
}

/// The three specifications, in the order a report lists them.
pub const SPECIFICATIONS: [Specification; 3] = [
    Specification::Ship,
    Specification::Spine,
    Specification::PairingService,
];

impl Specification {
    /// The name and version a test report gives the specification.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ship => "SHIP TS 1.0.0",
            Self::Spine => "SPINE TS 1.0.0",
            Self::PairingService => "SPS TestSpec 1.0.0",
        }
    }

    /// The prefix every identifier of the specification starts with.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Ship => "TC_SHIP_",
            Self::Spine => "TC_SPINE_",
            Self::PairingService => "TC_SPS_",
        }
    }

    /// Every case of the specification, in the order its mapping table lists them.
    pub fn cases(self) -> &'static [TestCase] {
        match self {
            Self::Ship => SHIP,
            Self::Spine => SPINE,
            Self::PairingService => PAIRING_SERVICE,
        }
    }

    /// The parameters the specification's parameter sheet defines, spelt as the sheet
    /// spells them.
    ///
    /// SPINE's `DUT Role` and `DUT Actor` and the Pairing Service's `PAR_dutRole` are
    /// parameters like any other: they select cases the same way.
    pub fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Ship => &[
                "PAR_shipSvc",
                "PAR_shipVersion",
                "PAR_helloProlongationCount",
                "PAR_initiateClose",
                "PAR_queryAccessMethods",
                "PAR_clientDetailedDiscoverySupported",
                "PAR_shipId",
            ],
            Self::Spine => &[
                "DUT Role",
                "DUT Actor",
                "PAR_initialTimeoutSupported",
                "PAR_addressChangeRecovery",
                "PAR_clientDetailedDiscoverySupported",
                "PAR_clientSubscriptionSupported",
                "PAR_spineVersion",
                "PAR_limitationUseCase",
                "PAR_lpcLpp_Test_Value1",
                "PAR_lpcLpp_Test_Value2",
            ],
            Self::PairingService => &[
                "PAR_dutRole",
                "PAR_typeRestrDevA",
                "PAR_spsSvcByDevA",
                "PAR_eccOfDevA - secp256r1",
                "PAR_eccOfDevA - brainpoolP256r1",
                "PAR_eccOfDevA - brainpoolP384r1",
                "PAR_ringBufferSize",
                "PAR_spsSvcDelTime",
                "PAR_spsSvcDelTime_DUR",
                "PAR_typeRestrDevZ",
                "PAR_spsSvcToDevZ",
                "PAR_parallelSpsSvcByDevZ",
                "PAR_spsSvcCorrection",
                "PAR_shipSvcByDevZ",
            ],
        }
    }

    /// The cases that apply to — and so are mandatory for — the device the sheet declares.
    pub fn mandatory(self, sheet: &ParameterSheet) -> impl Iterator<Item = &'static TestCase> {
        self.cases()
            .iter()
            .filter(move |case| case.applies_to(sheet))
    }

    /// How much of the specification a suite covers.
    ///
    /// Only the claims carrying this specification's [`prefix`](Self::prefix) are looked
    /// at, so one list of claims can be measured against all three; a claim with the prefix
    /// that names no case is reported by [`Coverage::unknown`].
    pub fn coverage(self, claimed: &[&str]) -> Coverage {
        let mine: Vec<&str> = claimed
            .iter()
            .copied()
            .filter(|id| id.starts_with(self.prefix()))
            .collect();
        Coverage::of_ids(self.cases().iter().map(|case| case.id), &mine)
    }
}

/// One case of a protocol-level test specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestCase {
    /// The `TC_…` identifier a test report names.
    pub id: &'static str,
    /// The specification the case belongs to.
    pub specification: Specification,
    /// The case's heading in the specification — its short description.
    pub title: &'static str,
    /// The DUT column of the specification's mapping table: `Server`, `Client`,
    /// `Server or client` or `Server & client` for SHIP; `Special`, `Server` or `Client`
    /// for SPINE; `devA` or `devZ` for the Pairing Service.
    pub dut: &'static str,
    /// The requirement identifiers the case verifies, without their brackets.
    ///
    /// Empty only for `TC_SHIP_AMDATA_004`, which the specification states as a safeguard
    /// for another case rather than as the test of a requirement.
    pub requirements: &'static [&'static str],
    /// The parameter-sheet conditions under which the case applies; all must hold.
    /// Empty means it applies to every device.
    pub applies_when: &'static [Condition],
}

impl TestCase {
    /// Whether the case applies to the device the sheet declares.
    pub fn applies_to(&self, sheet: &ParameterSheet) -> bool {
        self.applies_when
            .iter()
            .all(|condition| condition.holds(sheet))
    }

    /// Why this case is the device's to answer, if it is. See [`DEVICE_LEVEL`].
    pub fn owed_by_device(&self) -> Option<&'static DeviceCase> {
        DEVICE_LEVEL.iter().find(|case| case.id == self.id)
    }
}

/// A condition a parameter sheet has to meet for a case to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The parameter is declared with this value.
    Is(&'static str, &'static str),
    /// At least one of these `(parameter, value)` pairs is declared.
    AnyOf(&'static [(&'static str, &'static str)]),
    /// The parameter is declared as a whole number of at least this.
    AtLeast(&'static str, u32),
}

impl Condition {
    /// Whether the sheet meets the condition.
    pub fn holds(&self, sheet: &ParameterSheet) -> bool {
        match *self {
            Is(parameter, value) => sheet.declares(parameter, value),
            AnyOf(pairs) => pairs.iter().any(|(p, v)| sheet.declares(p, v)),
            AtLeast(parameter, minimum) => sheet
                .values(parameter)
                .any(|value| value.trim().parse::<u32>().is_ok_and(|n| n >= minimum)),
        }
    }

    /// The parameters the condition names.
    pub fn parameters(&self) -> impl Iterator<Item = &'static str> {
        let (single, pairs): (
            Option<&'static str>,
            &'static [(&'static str, &'static str)],
        ) = match *self {
            Is(parameter, _) | AtLeast(parameter, _) => (Some(parameter), &[]),
            AnyOf(pairs) => (None, pairs),
        };
        single.into_iter().chain(pairs.iter().map(|(p, _)| *p))
    }
}

/// What a device declares in a specification's parameter sheet.
///
/// A list of `(parameter, value)` pairs, spelt as the sheet spells them. A parameter with
/// more than one value — SPINE's `DUT Actor` for a device that is both an Energy Guard and
/// a Controllable System — is declared once per value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParameterSheet {
    declared: Vec<(String, String)>,
}

impl ParameterSheet {
    /// An empty sheet: only the cases with no condition apply.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a value for a parameter.
    pub fn declare(mut self, parameter: impl Into<String>, value: impl Into<String>) -> Self {
        self.declared.push((parameter.into(), value.into()));
        self
    }

    /// Whether the parameter is declared with this value.
    pub fn declares(&self, parameter: &str, value: &str) -> bool {
        self.values(parameter).any(|v| v == value)
    }

    /// Every value declared for the parameter.
    pub fn values<'a>(&'a self, parameter: &'a str) -> impl Iterator<Item = &'a str> {
        self.declared
            .iter()
            .filter(move |(p, _)| p == parameter)
            .map(|(_, v)| v.as_str())
    }

    /// Declared parameters the specification's sheet does not define.
    ///
    /// A misspelt parameter would otherwise quietly select fewer cases than a laboratory
    /// will run.
    pub fn unknown(&self, specification: Specification) -> Vec<&str> {
        let known = specification.parameters();
        let mut unknown: Vec<&str> = self
            .declared
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| !known.contains(p))
            .collect();
        unknown.dedup();
        unknown
    }
}

/// A case no library can answer, with the reason and a procedure for the device's harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCase {
    /// The `TC_…` identifier.
    pub id: &'static str,
    /// Why it is the device's, in the words [`device`] uses for every
    /// catalogue.
    pub reason: &'static str,
    /// What a consumer does against its own binary, and what should follow.
    ///
    /// A summary in this crate's words; the specification's own steps are authoritative.
    pub steps: &'static [Step],
}

/// The protocol-level cases that are the device's to answer.
pub static DEVICE_LEVEL: &[DeviceCase] = &[DeviceCase {
    id: "TC_SPS_DIGEST_003",
    reason: device::PERSISTENCE,
    steps: &[
        Step {
            action: "pair three distinct requesting SHIP instances with the device in turn, each \
                     through an accepted shippairing request, so that three digests enter its ring \
                     buffer",
            expected: "each pairing succeeds and a SHIP connection is established",
        },
        Step {
            action: "power-cycle the device",
            expected: "it comes back up",
        },
        Step {
            action: "announce the first instance's shippairing request again, unchanged",
            expected: "the device refuses every SHIP connection attempt from that instance: the \
                       digest survived the power cycle",
        },
        Step {
            action: "repeat with the second instance's request",
            expected: "refused in the same way",
        },
    ],
}];

/// Looks a case up by its identifier, across the three specifications.
pub fn find(id: &str) -> Option<&'static TestCase> {
    SPECIFICATIONS
        .iter()
        .flat_map(|spec| spec.cases())
        .find(|case| case.id == id)
}

static SHIP: &[TestCase] = &[
    TestCase {
        id: "TC_SHIP_MDNS_001",
        specification: Specification::Ship,
        title: "Validate mDNS TXT record",
        dut: "Server",
        requirements: &["SHIP-TS-MDNS-01", "SRIP-TS-TXT-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_CONN_001",
        specification: Specification::Ship,
        title: "Resolve simultaneous connections by SKI (DUT has larger SKI)",
        dut: "Server & client",
        requirements: &["SHIP-TS-CONN-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_ROLE_001",
        specification: Specification::Ship,
        title: "DUT as SME server",
        dut: "Server",
        requirements: &["SHIP-TS-ROLE-01", "SHIP-TS-CMI-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_ROLE_002",
        specification: Specification::Ship,
        title: "DUT as SME client",
        dut: "Client",
        requirements: &["SHIP-TS-ROLE-01", "SHIP-TS-CMI-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_ROLE_003",
        specification: Specification::Ship,
        title: "Simultaneous role polymorphism",
        dut: "Server & client",
        requirements: &["SHIP-TS-ROLE-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_SEC_001",
        specification: Specification::Ship,
        title: "Reject spoofed certificate (no prior pairing)",
        dut: "Server or client",
        requirements: &["SHIP-TS-SEC-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_SEC_002",
        specification: Specification::Ship,
        title: "Reject spoofed certificate (with prior pairing)",
        dut: "Server or client",
        requirements: &["SHIP-TS-SEC-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_MSG_001",
        specification: Specification::Ship,
        title: "Reject message without MessageValue",
        dut: "Server or client",
        requirements: &["SHIP-TS-MSG-01", "SHIP-TS-CMI-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_MSG_002",
        specification: Specification::Ship,
        title: "Reject unknown MessageType",
        dut: "Server or client",
        requirements: &["SHIP-TS-MSG-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_MSG_003",
        specification: Specification::Ship,
        title: "Support JSON whitespace formatting",
        dut: "Server or client",
        requirements: &["SHIP-IG-01", "SHIP-TS-ACC-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_CMI_001",
        specification: Specification::Ship,
        title: "Reject invalid MessageType (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-CMI-02"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_CMI_002",
        specification: Specification::Ship,
        title: "Reject invalid MessageType (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-CMI-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_CMI_003",
        specification: Specification::Ship,
        title: "Apply CmiTimeout (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-CMI-04"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_CMI_004",
        specification: Specification::Ship,
        title: "Apply CmiTimeout (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-CMI-04"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_CMI_005",
        specification: Specification::Ship,
        title: "Reject invalid CmiHead (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-CMI-03"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_CMI_006",
        specification: Specification::Ship,
        title: "Reject invalid CmiHead (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-CMI-03"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_HELLO_001",
        specification: Specification::Ship,
        title: "Process valid Hello & enter protocol handshake",
        dut: "Server",
        requirements: &["SHIP-TS-HELLO-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_HELLO_002",
        specification: Specification::Ship,
        title: "Accept prolongation requests",
        dut: "Server",
        requirements: &["SHIP-TS-HELLO-02"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_HELLO_003",
        specification: Specification::Ship,
        title: "Apply Wait-For-Ready-Timer (timeout)",
        dut: "Server",
        requirements: &["SHIP-TS-HELLO-03"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_HELLO_004",
        specification: Specification::Ship,
        title: "Ignore \"pending\" updates without prolongationRequest in \"ready\" state",
        dut: "Server",
        requirements: &["SHIP-TS-HELLO-04"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_PROT_001",
        specification: Specification::Ship,
        title: "Support JSON-UTF8 format (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-PROT-01"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_PROT_002",
        specification: Specification::Ship,
        title: "Support JSON-UTF8 format (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-PROT-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_PROT_003",
        specification: Specification::Ship,
        title: "Apply Wait-Timer (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-PROT-02"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_PROT_004",
        specification: Specification::Ship,
        title: "Apply Wait-Timer (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-PROT-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_PROT_005",
        specification: Specification::Ship,
        title: "Reject unexpected message (DUT as server)",
        dut: "Server",
        requirements: &["SHIP-TS-PROT-03"],
        applies_when: &[Is("PAR_shipSvc", "yes")],
    },
    TestCase {
        id: "TC_SHIP_PROT_006",
        specification: Specification::Ship,
        title: "Reject unexpected message (DUT as client)",
        dut: "Client",
        requirements: &["SHIP-TS-PROT-03"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_PIN_001",
        specification: Specification::Ship,
        title: "PIN state none",
        dut: "Server or client",
        requirements: &["SHIP-TS-PIN-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_TERM_001",
        specification: Specification::Ship,
        title: "Apply maxTime during termination (DUT initiates)",
        dut: "Server or client",
        requirements: &["SHIP-TS-TERM-01"],
        applies_when: &[Is("PAR_initiateClose", "yes")],
    },
    TestCase {
        id: "TC_SHIP_AM_001",
        specification: Specification::Ship,
        title: "Verify SHIP ID in access methods response",
        dut: "Server or client",
        requirements: &["SHIP-TS-DATA-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_AMDATA_001",
        specification: Specification::Ship,
        title: "Parallel processing of SME \"data\" while answering an \"access methods\" request",
        dut: "Server or client",
        requirements: &["SHIP-IG-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SHIP_AMDATA_002",
        specification: Specification::Ship,
        title: "Parallel processing of SME \"data\" while awaiting a delayed \"access methods\" response",
        dut: "Server or client",
        requirements: &["SHIP-IG-02"],
        applies_when: &[Is("PAR_queryAccessMethods", "yes")],
    },
    TestCase {
        id: "TC_SHIP_AMDATA_003",
        specification: Specification::Ship,
        title: "Parallel processing of SME \"access methods\" while awaiting a delayed \"data\" response",
        dut: "Server or client",
        requirements: &["SHIP-IG-03"],
        applies_when: &[Is("PAR_clientDetailedDiscoverySupported", "yes")],
    },
    TestCase {
        id: "TC_SHIP_AMDATA_004",
        specification: Specification::Ship,
        title: "Verify that a DUT declaring PAR_queryAccessMethods = \"no\" sends no \"access methods request\"",
        dut: "Server or client",
        requirements: &[],
        applies_when: &[Is("PAR_queryAccessMethods", "no")],
    },
];

static SPINE: &[TestCase] = &[
    TestCase {
        id: "TC_SPINE_COMP_001",
        specification: Specification::Spine,
        title: "Reject unknown Function",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_COMP_002",
        specification: Specification::Spine,
        title: "Forward version compatibility",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_COMP_003",
        specification: Specification::Spine,
        title: "Ignore unknown Elements",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-03"],
        applies_when: &[Is("PAR_clientDetailedDiscoverySupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_COMP_004",
        specification: Specification::Spine,
        title: "Ignore invalid replies",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-04"],
        applies_when: &[Is("PAR_clientDetailedDiscoverySupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_COMP_005",
        specification: Specification::Spine,
        title: "Strict version formatting",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-05"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_COMP_006",
        specification: Specification::Spine,
        title: "Version format variations",
        dut: "Special",
        requirements: &["SPINE-TS-COMP-06"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_001",
        specification: Specification::Spine,
        title: "Ascending message counters",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_002",
        specification: Specification::Spine,
        title: "Allow skipped counters",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_003",
        specification: Specification::Spine,
        title: "Handle message counter reset or overflow",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-03"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_004",
        specification: Specification::Spine,
        title: "Match reference counter",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-04"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_005",
        specification: Specification::Spine,
        title: "Acknowledge notify datagrams",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-05"],
        applies_when: &[Is("PAR_clientSubscriptionSupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_DATA_006",
        specification: Specification::Spine,
        title: "No response to results",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-06"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_007",
        specification: Specification::Spine,
        title: "Ignore ackRequest in read",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-07"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DATA_008",
        specification: Specification::Spine,
        title: "No response to results (DUT initiated)",
        dut: "Special",
        requirements: &["SPINE-TS-DATA-06"],
        applies_when: &[Is("PAR_clientDetailedDiscoverySupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_FC_001",
        specification: Specification::Spine,
        title: "Reject non-primary destinations",
        dut: "Special",
        requirements: &["SPINE-TS-FC-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_DDISC_001",
        specification: Specification::Spine,
        title: "Initial discovery of unknown partner",
        dut: "Special",
        requirements: &["SPINE-TS-DD-01"],
        applies_when: &[Is("PAR_clientDetailedDiscoverySupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_DDISC_002",
        specification: Specification::Spine,
        title: "Disconnect uncommunicative partner",
        dut: "Special",
        requirements: &["SPINE-TS-DD-02"],
        applies_when: &[Is("PAR_initialTimeoutSupported", "yes")],
    },
    TestCase {
        id: "TC_SPINE_BIND_001",
        specification: Specification::Spine,
        title: "Deny NodeManagement binding",
        dut: "Special",
        requirements: &["SPINE-TS-BIND-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_BIND_002",
        specification: Specification::Spine,
        title: "Reject unbound write",
        dut: "Server",
        requirements: &["SPINE-TS-BIND-02"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_SUBS_001",
        specification: Specification::Spine,
        title: "Accept NodeManagement subscription",
        dut: "Special",
        requirements: &["SPINE-TS-SUBS-01"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_SUBS_002",
        specification: Specification::Spine,
        title: "Idempotent subscription deletion",
        dut: "Special",
        requirements: &["SPINE-TS-SUBS-02"],
        applies_when: &[],
    },
    TestCase {
        id: "TC_SPINE_ENTITY_001",
        specification: Specification::Spine,
        title: "Dynamic server discovery",
        dut: "Client",
        requirements: &["SPINE-TS-ES-01"],
        applies_when: &[Is("DUT Actor", "EG")],
    },
    TestCase {
        id: "TC_SPINE_ENTITY_002",
        specification: Specification::Spine,
        title: "Dynamic server subscription",
        dut: "Client",
        requirements: &["SPINE-TS-ES-02"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTS_001",
        specification: Specification::Spine,
        title: "Tolerate arbitrary client Feature types",
        dut: "Server",
        requirements: &["SPINE-TS-RT-01", "SPINE-TS-DATA-05"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTS_002",
        specification: Specification::Spine,
        title: "Tolerate unknown Feature types",
        dut: "Server",
        requirements: &["SPINE-TS-COMP-02"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTS_003",
        specification: Specification::Spine,
        title: "Deduce client address from client's binding request",
        dut: "Server",
        requirements: &["SPINE-TS-RT-02"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTS_004",
        specification: Specification::Spine,
        title: "Ignore unknown payload Elements",
        dut: "Server",
        requirements: &["SPINE-TS-COMP-02", "SPINE-TS-COMP-03"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTS_005",
        specification: Specification::Spine,
        title: "Apply RFE merge logic ScaledNumberType",
        dut: "Server",
        requirements: &["SPINE-TS-RT-03"],
        applies_when: &[Is("DUT Actor", "CS")],
    },
    TestCase {
        id: "TC_SPINE_RTC_001",
        specification: Specification::Spine,
        title: "Ignore extra server Elements",
        dut: "Client",
        requirements: &["SPINE-TS-COMP-03"],
        applies_when: &[Is("DUT Actor", "EG")],
    },
    TestCase {
        id: "TC_SPINE_RTC_002",
        specification: Specification::Spine,
        title: "Ignore unknown server Elements",
        dut: "Client",
        requirements: &["SPINE-TS-COMP-02", "SPINE-TS-COMP-03"],
        applies_when: &[Is("DUT Actor", "EG")],
    },
    TestCase {
        id: "TC_SPINE_RTC_003",
        specification: Specification::Spine,
        title: "Ignore useCaseAvailable flag",
        dut: "Client",
        requirements: &["SPINE-TS-UCD-01"],
        applies_when: &[Is("DUT Actor", "EG")],
    },
];

static PAIRING_SERVICE: &[TestCase] = &[
    TestCase {
        id: "TC_SPS_DISC_001",
        specification: Specification::PairingService,
        title: "shippairing request announcement 1",
        dut: "devZ",
        requirements: &["SPS-REQ-5-001"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_DISC_002",
        specification: Specification::PairingService,
        title: "shippairing request announcement 2",
        dut: "devZ",
        requirements: &["SPS-REQ-4-002"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_DISC_003",
        specification: Specification::PairingService,
        title: "shippairing request deletion time 1",
        dut: "devZ",
        requirements: &["SPS-REQ-4-003"],
        applies_when: &[
            Is("PAR_dutRole", "devZ"),
            Is("PAR_spsSvcDelTime", "Option 1"),
        ],
    },
    TestCase {
        id: "TC_SPS_DISC_004",
        specification: Specification::PairingService,
        title: "shippairing request deletion time 2",
        dut: "devZ",
        requirements: &["SPS-REQ-4-003"],
        applies_when: &[
            Is("PAR_dutRole", "devZ"),
            Is("PAR_spsSvcDelTime", "Option 2"),
        ],
    },
    TestCase {
        id: "TC_SPS_DISC_005",
        specification: Specification::PairingService,
        title: "shippairing request deletion time 3",
        dut: "devZ",
        requirements: &["SPS-REQ-4-003"],
        applies_when: &[
            Is("PAR_dutRole", "devZ"),
            Is("PAR_spsSvcDelTime", "Option 2"),
            AtLeast("PAR_spsSvcDelTime_DUR", 2),
        ],
    },
    TestCase {
        id: "TC_SPS_DISC_006",
        specification: Specification::PairingService,
        title: "Immediate deletion of service upon revocation of trust",
        dut: "devZ",
        requirements: &["SPS-REQ-4-004"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_DISC_007",
        specification: Specification::PairingService,
        title: "Verification of trust revocation",
        dut: "devZ",
        requirements: &["SPS-REQ-4-004"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_DISC_008",
        specification: Specification::PairingService,
        title: "Validation of TTL delay for reused instance name",
        dut: "devZ",
        requirements: &["SPS-REQ-5-022", "SPS-REQ-5-023"],
        applies_when: &[
            Is("PAR_dutRole", "devZ"),
            Is("PAR_spsSvcCorrection", "Option 2"),
        ],
    },
    TestCase {
        id: "TC_SPS_DISC_009",
        specification: Specification::PairingService,
        title: "Correction of wrong shippairing requests",
        dut: "devZ",
        requirements: &["SPS-REQ-4-013"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_TXT_001",
        specification: Specification::PairingService,
        title: "Announcement of valid TXT record",
        dut: "devZ",
        requirements: &[
            "SPS-REQ-4-002",
            "SPS-REQ-5-008",
            "SPS-REQ-5-009",
            "SPS-REQ-5-010",
            "SPS-REQ-5-011",
            "SPS-REQ-6-003",
            "SPS-REQ-6-005",
            "SPS-REQ-6-010",
        ],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_TXT_002",
        specification: Specification::PairingService,
        title: "Acceptance of rearranged TXT record",
        dut: "devA",
        requirements: &[
            "SPS-REQ-4-005",
            "SPS-REQ-5-009",
            "SPS-REQ-7-002",
            "SPS-REQ-9-002",
        ],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TXT_003",
        specification: Specification::PairingService,
        title: "Rejection of incomplete TXT record",
        dut: "devA",
        requirements: &[
            "SPS-REQ-4-006",
            "SPS-REQ-5-010",
            "SPS-REQ-5-014",
            "SPS-REQ-5-016",
            "SPS-REQ-9-002",
        ],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TXT_004",
        specification: Specification::PairingService,
        title: "Rejection of malformed TXT record 1",
        dut: "devA",
        requirements: &[
            "SPS-REQ-4-006",
            "SPS-REQ-5-011",
            "SPS-REQ-5-015",
            "SPS-REQ-6-005",
            "SPS-REQ-9-002",
        ],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TXT_005",
        specification: Specification::PairingService,
        title: "Ignore unknown keys",
        dut: "devA",
        requirements: &["SPS-REQ-4-005", "SPS-REQ-5-013"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TXT_006",
        specification: Specification::PairingService,
        title: "Reject unsupported elliptic curve (trustCurve)",
        dut: "devA",
        requirements: &["SPS-REQ-4-006", "SPS-REQ-5-017", "SPS-REQ-9-002"],
        applies_when: &[
            Is("PAR_dutRole", "devA"),
            AnyOf(&[
                ("PAR_eccOfDevA - brainpoolP256r1", "no"),
                ("PAR_eccOfDevA - brainpoolP384r1", "no"),
            ]),
        ],
    },
    TestCase {
        id: "TC_SPS_TXT_007",
        specification: Specification::PairingService,
        title: "Prevent spoofing from TXT record with duplicate trustPar",
        dut: "devA",
        requirements: &["SPS-REQ-4-006"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TXT_008",
        specification: Specification::PairingService,
        title: "Prevention of secret leakage in TXT record",
        dut: "devZ",
        requirements: &["SPS-REQ-5-018", "SPS-REQ-5-019", "SPS-REQ-5-020"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_TXT_009",
        specification: Specification::PairingService,
        title: "Rejection of malformed TXT record 2",
        dut: "devA",
        requirements: &[
            "SPS-REQ-4-006",
            "SPS-REQ-5-011",
            "SPS-REQ-5-015",
            "SPS-REQ-6-005",
            "SPS-REQ-9-002",
        ],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_DIGEST_001",
        specification: Specification::PairingService,
        title: "Digest verification",
        dut: "devA",
        requirements: &["SPS-REQ-4-005", "SPS-REQ-9-004"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_DIGEST_002",
        specification: Specification::PairingService,
        title: "Ring buffer properties and blocking of replay attempts",
        dut: "devA",
        requirements: &["SPS-REQ-9-003", "SPS-REQ-9-005", "SPS-REQ-11-001"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_DIGEST_003",
        specification: Specification::PairingService,
        title: "Ring buffer persistence",
        dut: "devA",
        requirements: &["SPS-REQ-9-005", "SPS-REQ-11-002"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_001",
        specification: Specification::PairingService,
        title: "Single control unit rule 1",
        dut: "devA",
        requirements: &["SPS-REQ-4-007", "SPS-REQ-4-008", "SPS-REQ-4-009"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_002",
        specification: Specification::PairingService,
        title: "Single control unit rule 2",
        dut: "devA",
        requirements: &["SPS-REQ-4-008", "SPS-REQ-4-009", "SPS-REQ-4-016"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_003",
        specification: Specification::PairingService,
        title: "addCu-request process (listener) reactivation after communication loss",
        dut: "devA",
        requirements: &["SPS-REQ-4-008", "SPS-REQ-4-010"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_004",
        specification: Specification::PairingService,
        title: "Evaluation of corrected request",
        dut: "devA",
        requirements: &["SPS-REQ-4-005"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_005",
        specification: Specification::PairingService,
        title: "Sending of parallel shippairing requests",
        dut: "devZ",
        requirements: &["SPS-REQ-4-012", "SPS-REQ-5-003"],
        applies_when: &[
            Is("PAR_dutRole", "devZ"),
            Is("PAR_parallelSpsSvcByDevZ", "yes"),
        ],
    },
    TestCase {
        id: "TC_SPS_PROC_006",
        specification: Specification::PairingService,
        title: "Sequential processing of parallel shippairing requests",
        dut: "devA",
        requirements: &["SPS-REQ-9-001"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PROC_007",
        specification: Specification::PairingService,
        title: "SHIP connection attempt before shippairing request",
        dut: "devZ",
        requirements: &["SPS-REQ-4-001"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_PROC_008",
        specification: Specification::PairingService,
        title: "SHIP connection acceptance before shippairing request",
        dut: "devZ",
        requirements: &["SPS-REQ-4-001"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_PROC_009",
        specification: Specification::PairingService,
        title: "addCu-request process (listener) abortion upon re-established communication",
        dut: "devA",
        requirements: &["SPS-REQ-4-011"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TRUST_001",
        specification: Specification::PairingService,
        title: "devA SHIP communication after accepted shippairing request",
        dut: "devA",
        requirements: &["SPS-REQ-4-005", "SPS-REQ-4-007", "SPS-REQ-6-004"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_TRUST_002",
        specification: Specification::PairingService,
        title: "Removal of trust for previous control unit",
        dut: "devA",
        requirements: &["SPS-REQ-4-010", "SPS-REQ-10-002"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_QR_001",
        specification: Specification::PairingService,
        title: "Mandatory QR code keys for SHIP Pairing Service",
        dut: "devA",
        requirements: &[
            "SPS-REQ-6-004",
            "SPS-REQ-6-005",
            "SPS-REQ-12-001",
            "SPS-REQ-12-002",
        ],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_FP_001",
        specification: Specification::PairingService,
        title: "Rejection of invalid fingerprint of devA",
        dut: "devZ",
        requirements: &["SPS-REQ-10-001"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_FP_002",
        specification: Specification::PairingService,
        title: "Rejection of invalid fingerprint of devZ",
        dut: "devA",
        requirements: &["SPS-REQ-10-001"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PARA_001",
        specification: Specification::PairingService,
        title: "No shippairing service",
        dut: "devA",
        requirements: &["SPS-REQ-4-015"],
        applies_when: &[Is("PAR_dutRole", "devA"), Is("PAR_spsSvcByDevA", "no")],
    },
    TestCase {
        id: "TC_SPS_PARA_002",
        specification: Specification::PairingService,
        title: "Only addCu-requests",
        dut: "devZ",
        requirements: &["SPS-REQ-4-014"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_PARA_003",
        specification: Specification::PairingService,
        title: "trustNonce variation",
        dut: "devZ",
        requirements: &["SPS-REQ-5-012", "SPS-REQ-6-011", "SPS-REQ-6-012"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
    TestCase {
        id: "TC_SPS_PARA_004",
        specification: Specification::PairingService,
        title: "No connection attempt to shippairing port",
        dut: "devA",
        requirements: &["SPS-REQ-5-007"],
        applies_when: &[Is("PAR_dutRole", "devA")],
    },
    TestCase {
        id: "TC_SPS_PARA_005",
        specification: Specification::PairingService,
        title: "No parallel shipparing requests to the same devA",
        dut: "devZ",
        requirements: &["SPS-REQ-4-013"],
        applies_when: &[Is("PAR_dutRole", "devZ")],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The three mapping tables list 33, 31 and 40 cases; so do the parameter sheets.
    #[test]
    fn every_specification_holds_its_published_count() {
        let counts: Vec<usize> = SPECIFICATIONS.iter().map(|s| s.cases().len()).collect();
        assert_eq!(counts, [33, 31, 40], "SHIP, SPINE, Pairing Service");
    }

    #[test]
    fn identifiers_are_unique_and_carry_their_specifications_prefix() {
        let mut seen: Vec<&str> = Vec::new();
        for spec in SPECIFICATIONS {
            for case in spec.cases() {
                assert_eq!(case.specification, spec, "{}", case.id);
                let rest = case
                    .id
                    .strip_prefix(spec.prefix())
                    .unwrap_or_else(|| panic!("{} lacks {}", case.id, spec.prefix()));
                let (group, number) = rest.split_once('_').expect("GROUP_NNN");
                assert!(
                    !group.is_empty() && group.bytes().all(|b| b.is_ascii_uppercase()),
                    "{}",
                    case.id
                );
                assert!(
                    number.len() == 3 && number.bytes().all(|b| b.is_ascii_digit()),
                    "{}",
                    case.id
                );
                assert!(!seen.contains(&case.id), "duplicate {}", case.id);
                assert!(
                    !case.title.is_empty() && !case.dut.is_empty(),
                    "{}",
                    case.id
                );
                assert_eq!(find(case.id), Some(case));
                seen.push(case.id);
            }
        }
        let without: Vec<&str> = SPECIFICATIONS
            .iter()
            .flat_map(|s| s.cases())
            .filter(|case| case.requirements.is_empty())
            .map(|case| case.id)
            .collect();
        assert_eq!(without, ["TC_SHIP_AMDATA_004"]);
    }

    /// The DUT column and the applicability agree: SPINE's `Special` cases apply to any
    /// actor and the others to exactly one; every Pairing Service case names its role.
    #[test]
    fn the_dut_column_agrees_with_the_applicability() {
        for case in Specification::Spine.cases() {
            let actor = case
                .applies_when
                .iter()
                .find(|c| matches!(c, Is("DUT Actor", _)));
            assert_eq!(case.dut == "Special", actor.is_none(), "{}", case.id);
        }
        for case in Specification::PairingService.cases() {
            assert!(
                case.applies_when.contains(&Is("PAR_dutRole", case.dut)),
                "{}",
                case.id
            );
        }
    }

    #[test]
    fn every_condition_names_a_parameter_of_its_sheet() {
        for spec in SPECIFICATIONS {
            for case in spec.cases() {
                for parameter in case.applies_when.iter().flat_map(|c| c.parameters()) {
                    assert!(
                        spec.parameters().contains(&parameter),
                        "{} names {parameter}, which the {} sheet does not define",
                        case.id,
                        spec.name()
                    );
                }
            }
        }
    }

    #[test]
    fn a_misspelt_parameter_is_reported() {
        let sheet = ParameterSheet::new()
            .declare("PAR_shipSvc", "yes")
            .declare("PAR_shipSvcc", "yes");
        assert_eq!(sheet.unknown(Specification::Ship), ["PAR_shipSvcc"]);
        assert!(
            ParameterSheet::new()
                .declare("PAR_dutRole", "devA")
                .unknown(Specification::PairingService)
                .is_empty()
        );
    }

    #[test]
    fn a_declaration_moves_cases_between_not_applicable_and_mandatory() {
        let count = |spec: Specification, sheet: &ParameterSheet| spec.mandatory(sheet).count();

        // SHIP: fourteen cases need the device to announce a SHIP server.
        let client_only = ParameterSheet::new().declare("PAR_shipSvc", "no");
        let server = ParameterSheet::new().declare("PAR_shipSvc", "yes");
        assert_eq!(
            count(Specification::Ship, &server) - count(Specification::Ship, &client_only),
            14
        );
        let hello = find("TC_SHIP_HELLO_002").unwrap();
        assert!(!hello.applies_to(&client_only) && hello.applies_to(&server));

        // PAR_queryAccessMethods selects one of two complementary cases.
        let asks = ParameterSheet::new().declare("PAR_queryAccessMethods", "yes");
        let silent = ParameterSheet::new().declare("PAR_queryAccessMethods", "no");
        let two = find("TC_SHIP_AMDATA_002").unwrap();
        let four = find("TC_SHIP_AMDATA_004").unwrap();
        assert!(two.applies_to(&asks) && !four.applies_to(&asks));
        assert!(!two.applies_to(&silent) && four.applies_to(&silent));

        // SPINE: a device that is both actors gets both actors' cases.
        let cs = ParameterSheet::new().declare("DUT Actor", "CS");
        let both = cs.clone().declare("DUT Actor", "EG");
        let rtc = find("TC_SPINE_RTC_001").unwrap();
        assert!(!rtc.applies_to(&cs) && rtc.applies_to(&both));

        // Pairing Service: an unsupported curve raises the trustCurve case; a short
        // deletion time drops the one that needs two minutes of margin.
        let dev_a = ParameterSheet::new().declare("PAR_dutRole", "devA");
        let curve = find("TC_SPS_TXT_006").unwrap();
        assert!(!curve.applies_to(&dev_a));
        assert!(
            curve.applies_to(
                &dev_a
                    .clone()
                    .declare("PAR_eccOfDevA - brainpoolP384r1", "no")
            )
        );
        let dev_z = ParameterSheet::new()
            .declare("PAR_dutRole", "devZ")
            .declare("PAR_spsSvcDelTime", "Option 2");
        let margin = find("TC_SPS_DISC_005").unwrap();
        assert!(!margin.applies_to(&dev_z.clone().declare("PAR_spsSvcDelTime_DUR", "1")));
        assert!(margin.applies_to(&dev_z.declare("PAR_spsSvcDelTime_DUR", "15")));

        // The two roles split the Pairing Service between them.
        assert_eq!(
            count(Specification::PairingService, &ParameterSheet::new()),
            0
        );
    }

    #[test]
    fn the_device_level_case_resolves_and_has_steps() {
        assert_eq!(DEVICE_LEVEL.len(), 1);
        for owed in DEVICE_LEVEL {
            let case = find(owed.id).expect("a real case");
            assert_eq!(case.owed_by_device(), Some(owed));
            assert!(!owed.steps.is_empty());
        }
        assert!(
            find("TC_SPS_DIGEST_002")
                .unwrap()
                .owed_by_device()
                .is_none()
        );
    }

    #[test]
    fn coverage_looks_only_at_its_own_specifications_claims() {
        let claims = ["TC_SHIP_HELLO_002", "TC_SPINE_DATA_001", "TC_SHIP_NOPE_001"];
        let ship = Specification::Ship.coverage(&claims);
        assert_eq!(ship.covered(), 1);
        assert_eq!(ship.missing(), 32);
        assert_eq!(ship.unknown(), ["TC_SHIP_NOPE_001"]);
        let sps = Specification::PairingService.coverage(&claims);
        assert_eq!((sps.covered(), sps.missing()), (0, 40));
        assert!(sps.unknown().is_empty());
    }
}
