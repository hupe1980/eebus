//! Service discovery: the `_ship._tcp` mDNS record.
//!
//! A SHIP node announces itself over mDNS-SD with a TXT record that carries everything
//! a peer needs to decide whether to connect and how: the node's identity ([`Ski`]), its
//! stable [`ShipId`], the WebSocket path, and enough description for a person to
//! recognise the device in a list.
//!
//! The keys follow SHIP §5.4 as amended by the *SHIP Requirements for Installation
//! Process* 1.1.0, which adds `serial` and `cat` and makes the SKI uppercase.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use super::Ski;

/// The DNS-SD service type SHIP nodes announce.
pub const SERVICE_TYPE: &str = "_ship._tcp";

/// The DNS-SD service type of the SHIP Pairing Service.
pub const PAIRING_SERVICE_TYPE: &str = "_shippairing._tcp";

/// The only TXT record version this specification defines.
pub const TXTVERS: &str = "1";

/// A SHIP ID: the stable name of a node, independent of its certificate.
///
/// The installation requirements (`SRIP-220/5`) define the format
/// `i:<IANA PEN>_u:<vendor product id>` for newly built devices, which makes the ID
/// globally unique without a registry. Devices already in the field keep whatever ID
/// they shipped with, and `SRIP-220/9` forbids treating a different format as an error —
/// so this type accepts any non-empty value and only *offers* the structured form.
///
/// ```
/// use eebus::ship::ShipId;
///
/// let id = ShipId::new("12345", "C8277H008F-3");
/// assert_eq!(id.as_str(), "i:12345_u:C8277H008F-3");
/// assert_eq!(id.iana_pen(), Some("12345"));
///
/// // A legacy identifier is still a valid identifier.
/// let legacy: ShipId = "ExampleBrand-Dishwasher-1".parse().unwrap();
/// assert_eq!(legacy.iana_pen(), None);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShipId(String);

/// Why a string could not be read as a [`ShipId`] or a TXT record.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// A value was empty where the specification requires content.
    #[error("`{0}` must not be empty")]
    Empty(&'static str),
    /// A value contained a character the specification prohibits.
    #[error("`{key}` contains the prohibited character {ch:?}")]
    ProhibitedCharacter {
        /// The key whose value was rejected.
        key: &'static str,
        /// The offending character.
        ch: char,
    },
    /// A value exceeded the length the specification allows.
    #[error("`{key}` is limited to {max} characters, found {found}")]
    TooLong {
        /// The key whose value was rejected.
        key: &'static str,
        /// The permitted length.
        max: usize,
        /// The length found.
        found: usize,
    },
    /// A mandatory key was missing from a received record.
    #[error("mandatory TXT key `{0}` is missing")]
    MissingKey(&'static str),
    /// `txtvers` named a version this implementation does not understand.
    ///
    /// SHIP §5.4 says such records are discarded silently rather than reported.
    #[error("unsupported TXT record version `{0}`")]
    UnsupportedVersion(String),
    /// The `ski` key did not hold a valid SKI.
    #[error("invalid SKI: {0}")]
    Ski(#[from] super::SkiParseError),
    /// The `cat` key did not hold a comma-separated list of category numbers.
    #[error("invalid device category list `{0}`")]
    Categories(String),
}

impl ShipId {
    /// Builds the structured identifier `i:<pen>_u:<product id>`.
    pub fn new(iana_pen: &str, product_id: &str) -> Self {
        Self(alloc::format!("i:{iana_pen}_u:{product_id}"))
    }

    /// The identifier as it appears on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The IANA Private Enterprise Number, for identifiers in the structured form.
    pub fn iana_pen(&self) -> Option<&str> {
        self.0.strip_prefix("i:")?.split_once("_u:").map(|(p, _)| p)
    }

    /// The vendor product identifier, for identifiers in the structured form.
    pub fn product_id(&self) -> Option<&str> {
        self.0.strip_prefix("i:")?.split_once("_u:").map(|(_, u)| u)
    }
}

impl FromStr for ShipId {
    type Err = DiscoveryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        check_value("id", s, 63)?;
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for ShipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of device this is, for the `cat` TXT key.
///
/// The numbers come from Table 1 of the installation requirements and are fixed: later
/// versions may append categories but never renumber them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum DeviceCategory {
    /// A control unit belonging to the grid operator.
    GridConnectionPointHub = 1,
    /// An energy management system.
    EnergyManagementSystem = 2,
    /// Charging stations and other e-mobility equipment.
    EMobility = 3,
    /// Heat pumps and other HVAC equipment.
    Hvac = 4,
    /// PV, battery and hybrid inverters.
    Inverter = 5,
    /// Washing machines, dryers, fridges and the like.
    DomesticAppliance = 6,
    /// Meters and sub-meters with their own communication.
    MeteringDevice = 7,
}

impl DeviceCategory {
    /// Reads a category number.
    pub fn from_number(n: u8) -> Option<Self> {
        Some(match n {
            1 => Self::GridConnectionPointHub,
            2 => Self::EnergyManagementSystem,
            3 => Self::EMobility,
            4 => Self::Hvac,
            5 => Self::Inverter,
            6 => Self::DomesticAppliance,
            7 => Self::MeteringDevice,
            _ => return None,
        })
    }

    /// The category number.
    pub const fn number(self) -> u8 {
        self as u8
    }
}

/// The TXT record of a `_ship._tcp` service.
///
/// ```
/// use eebus::ship::{DeviceCategory, ShipTxtRecord, ShipId};
///
/// let record = ShipTxtRecord::new(
///     ShipId::new("12345", "HEMS-0001"),
///     "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap(),
/// )
/// .with_brand("ExampleBrand")
/// .with_model("E1234")
/// .with_categories([DeviceCategory::EnergyManagementSystem]);
///
/// let pairs = record.to_pairs().unwrap();
/// assert_eq!(pairs[0], ("txtvers".to_string(), "1".to_string()));
/// assert!(pairs.contains(&("cat".to_string(), "2".to_string())));
///
/// let parsed = ShipTxtRecord::from_pairs(&pairs).unwrap();
/// assert_eq!(parsed, record);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShipTxtRecord {
    /// The node's stable identifier.
    pub id: ShipId,
    /// The WebSocket path to connect to, conventionally `/ship/`.
    pub path: String,
    /// The node's Subject Key Identifier.
    pub ski: Ski,
    /// Whether the node is currently in "auto accept" registration mode.
    ///
    /// The SHIP implementation guide §2.3 forbids that mode outright, so a compliant
    /// node always announces `false`; the field exists because peers in the field still
    /// send it.
    pub register: bool,
    /// Whether the node supports elliptic curves beyond `secp256r1`, mandatory from
    /// SHIP 1.1.
    pub ecc: Option<bool>,
    /// The brand shown to a user.
    pub brand: Option<String>,
    /// A human-readable device type, such as `Heatpump`.
    pub device_type: Option<String>,
    /// The product model.
    pub model: Option<String>,
    /// The serial number printed on the device.
    pub serial: Option<String>,
    /// The categories that apply to this device.
    pub categories: Vec<DeviceCategory>,
}

impl ShipTxtRecord {
    /// A record with the mandatory keys and safe defaults.
    pub fn new(id: ShipId, ski: Ski) -> Self {
        Self {
            id,
            path: super::DEFAULT_PATH.to_string(),
            ski,
            register: false,
            ecc: Some(false),
            brand: None,
            device_type: None,
            model: None,
            serial: None,
            categories: Vec::new(),
        }
    }

    /// Sets the brand.
    #[must_use]
    pub fn with_brand(mut self, brand: &str) -> Self {
        self.brand = Some(brand.to_owned());
        self
    }

    /// Sets the human-readable device type.
    #[must_use]
    pub fn with_device_type(mut self, device_type: &str) -> Self {
        self.device_type = Some(device_type.to_owned());
        self
    }

    /// Sets the model.
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = Some(model.to_owned());
        self
    }

    /// Sets the serial number.
    #[must_use]
    pub fn with_serial(mut self, serial: &str) -> Self {
        self.serial = Some(serial.to_owned());
        self
    }

    /// Sets the device categories.
    #[must_use]
    pub fn with_categories(mut self, categories: impl IntoIterator<Item = DeviceCategory>) -> Self {
        self.categories = categories.into_iter().collect();
        self.categories.sort_unstable();
        self.categories.dedup();
        self
    }

    /// Renders the record as ordered key/value pairs, validating every value.
    ///
    /// `txtvers` comes first, as SHIP §5.4 requires.
    pub fn to_pairs(&self) -> Result<Vec<(String, String)>, DiscoveryError> {
        let mut out = Vec::with_capacity(10);
        out.push(("txtvers".to_string(), TXTVERS.to_string()));

        check_value("id", self.id.as_str(), 63)?;
        out.push(("id".to_string(), self.id.as_str().to_string()));

        check_value("path", &self.path, 255)?;
        out.push(("path".to_string(), self.path.clone()));

        out.push(("ski".to_string(), self.ski.to_txt_value()));
        out.push((
            "register".to_string(),
            if self.register { "true" } else { "false" }.to_string(),
        ));
        if let Some(ecc) = self.ecc {
            out.push((
                "ecc".to_string(),
                if ecc { "true" } else { "false" }.to_string(),
            ));
        }

        for (key, value) in [
            ("brand", &self.brand),
            ("type", &self.device_type),
            ("model", &self.model),
            ("serial", &self.serial),
        ] {
            if let Some(value) = value {
                check_value(key, value, 32)?;
                out.push((key.to_string(), value.clone()));
            }
        }

        if !self.categories.is_empty() {
            let cat = self
                .categories
                .iter()
                .map(|c| c.number().to_string())
                .collect::<Vec<_>>()
                .join(",");
            out.push(("cat".to_string(), cat));
        }
        Ok(out)
    }

    /// Reads a record from the key/value pairs of a discovered service.
    ///
    /// Unknown keys are ignored, and the optional keys are treated as optional even
    /// though the installation requirements make some of them mandatory *to send*:
    /// `SRIP-220/20`, `/22` and `/24` are explicit that a peer must not treat their
    /// absence as an error, because devices predating those rules are still in service.
    pub fn from_pairs<K: AsRef<str>, V: AsRef<str>>(
        pairs: &[(K, V)],
    ) -> Result<Self, DiscoveryError> {
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k.as_ref().eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_ref())
        };

        match get("txtvers") {
            Some(TXTVERS) => {}
            Some(other) => return Err(DiscoveryError::UnsupportedVersion(other.to_owned())),
            None => return Err(DiscoveryError::MissingKey("txtvers")),
        }

        let id: ShipId = get("id").ok_or(DiscoveryError::MissingKey("id"))?.parse()?;
        let ski: Ski = get("ski")
            .ok_or(DiscoveryError::MissingKey("ski"))?
            .parse()?;

        let categories = match get("cat") {
            None => Vec::new(),
            Some(raw) => {
                let mut out = Vec::new();
                for part in raw.split(',').filter(|p| !p.is_empty()) {
                    let n: u8 = part
                        .trim()
                        .parse()
                        .map_err(|_| DiscoveryError::Categories(raw.to_owned()))?;
                    let cat = DeviceCategory::from_number(n)
                        .ok_or_else(|| DiscoveryError::Categories(raw.to_owned()))?;
                    if !out.contains(&cat) {
                        out.push(cat);
                    }
                }
                out
            }
        };

        Ok(Self {
            id,
            path: get("path").unwrap_or(super::DEFAULT_PATH).to_owned(),
            ski,
            register: get("register").is_some_and(|v| v.eq_ignore_ascii_case("true")),
            ecc: get("ecc").map(|v| v.eq_ignore_ascii_case("true")),
            brand: get("brand")
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
            device_type: get("type").filter(|v| !v.is_empty()).map(ToOwned::to_owned),
            model: get("model")
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
            serial: get("serial")
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
            categories,
        })
    }
}

/// Rejects the characters `SRIP-220/1..3` prohibit in TXT values: the semicolon, which
/// separates fields in the QR code, and anything in Unicode's control, format or
/// separator categories — whitespace included.
fn check_value(key: &'static str, value: &str, max: usize) -> Result<(), DiscoveryError> {
    if value.is_empty() {
        return Err(DiscoveryError::Empty(key));
    }
    if value.chars().count() > max {
        return Err(DiscoveryError::TooLong {
            key,
            max,
            found: value.chars().count(),
        });
    }
    for ch in value.chars() {
        if ch == ';' || ch.is_control() || ch.is_whitespace() {
            return Err(DiscoveryError::ProhibitedCharacter { key, ch });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> ShipTxtRecord {
        ShipTxtRecord::new(
            ShipId::new("12345", "123abc456def"),
            "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap(),
        )
        .with_brand("ExampleBrand")
        .with_device_type("Heatpump")
        .with_model("E1234")
        .with_serial("123abc456def")
        .with_categories([DeviceCategory::Hvac])
    }

    /// `TC_SHIP_MDNS_001`: the announced record carries every mandatory key, with
    /// `txtvers` first and the SKI in uppercase (SHIP 1.1.0 §5.4).
    #[test]
    fn tc_ship_mdns_001_mandatory_keys_are_present() {
        let pairs = example().to_pairs().unwrap();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys[0], "txtvers");
        for mandatory in ["txtvers", "id", "path", "ski", "register", "ecc"] {
            assert!(keys.contains(&mandatory), "missing {mandatory}");
        }

        let find = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(find("id"), "i:12345_u:123abc456def");
        assert_eq!(find("path"), "/ship/");
        assert_eq!(find("ski"), "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222");
        assert_eq!(find("cat"), "4");
    }

    /// The SHIP implementation guide §2.3 forbids "auto accept", so `register` is never
    /// announced as true.
    #[test]
    fn register_defaults_to_false() {
        let pairs = example().to_pairs().unwrap();
        assert!(pairs.contains(&("register".to_string(), "false".to_string())));
    }

    #[test]
    fn records_round_trip() {
        let record = example();
        assert_eq!(
            ShipTxtRecord::from_pairs(&record.to_pairs().unwrap()).unwrap(),
            record
        );
    }

    #[test]
    fn optional_keys_may_be_absent() {
        let minimal = [
            ("txtvers", "1"),
            ("id", "LegacyDevice-1"),
            ("path", "/ship/"),
            ("ski", "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222"),
            ("register", "false"),
        ];
        let record = ShipTxtRecord::from_pairs(&minimal).unwrap();
        assert_eq!(record.brand, None);
        assert_eq!(record.ecc, None);
        assert!(record.categories.is_empty());
        assert_eq!(record.id.iana_pen(), None, "a legacy identifier");
    }

    #[test]
    fn unknown_txtvers_is_reported_so_the_caller_can_discard_the_record() {
        let pairs = [("txtvers", "2"), ("id", "x"), ("ski", "00")];
        assert!(matches!(
            ShipTxtRecord::from_pairs(&pairs),
            Err(DiscoveryError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn prohibited_characters_are_rejected() {
        let record = example().with_brand("Example Brand");
        assert!(matches!(
            record.to_pairs(),
            Err(DiscoveryError::ProhibitedCharacter {
                key: "brand",
                ch: ' '
            })
        ));

        let record = example().with_model("A;B");
        assert!(matches!(
            record.to_pairs(),
            Err(DiscoveryError::ProhibitedCharacter {
                key: "model",
                ch: ';'
            })
        ));
    }

    #[test]
    fn overlong_values_are_rejected() {
        let record = example().with_serial(&"x".repeat(33));
        assert!(matches!(
            record.to_pairs(),
            Err(DiscoveryError::TooLong {
                key: "serial",
                max: 32,
                found: 33
            })
        ));
    }

    #[test]
    fn categories_are_deduplicated_and_ordered() {
        let record = example().with_categories([
            DeviceCategory::Inverter,
            DeviceCategory::EnergyManagementSystem,
            DeviceCategory::Inverter,
        ]);
        let pairs = record.to_pairs().unwrap();
        assert!(pairs.contains(&("cat".to_string(), "2,5".to_string())));
    }
}
