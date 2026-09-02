//! The SHIP installation QR code.
//!
//! Commissioning a §14a installation means telling two devices about each other's keys
//! before either will talk. The QR code on the device — or on its display, since a
//! certificate update changes the SKI — carries exactly what an installer's tool needs
//! to do that without typing forty hex digits.
//!
//! The grammar is fixed by *SHIP Requirements for Installation Process* §3.1:
//!
//! ```text
//! SHIP;SKI:<ski>;[PIN:<pin>;]ID:<ship id>;[BRAND:…;][TYPE:…;][MODEL:…;]
//! [SERIAL:…;][CAT:…;][NOMINALPOWER:<production>,<consumption>;]…ENDSHIP;
//! ```
//!
//! Unknown keys are skipped rather than rejected (`SRIP-310/15`), because later
//! versions are expected to add some, and `ENDSHIP;` is optional on input
//! (`SRIP-310/17`) since QR codes predating the marker are in the field.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use super::{DeviceCategory, DiscoveryError, ShipId, Ski};

/// The contents of a SHIP installation QR code.
///
/// ```
/// use eebus::ship::ShipQr;
///
/// // The worked example from the installation requirements, §3.1.
/// let text = "SHIP;SKI:5555 AAAA FFFF 1111 CCCC 3333 EEEE DDDD 9999 2222;\
///             PIN:5555 AAAA FF;ID:i:12345_u:123abc456def;BRAND:ExampleBrand;\
///             TYPE:Heatpump;MODEL:E1234;SERIAL:123abc456def;CAT:4;\
///             NOMINALPOWER:0,11000;ENDSHIP;";
///
/// let qr: ShipQr = text.parse().unwrap();
/// assert_eq!(qr.ski.to_txt_value(), "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222");
/// assert_eq!(qr.pin.as_deref(), Some("5555AAAAFF"));
/// assert_eq!(qr.brand.as_deref(), Some("ExampleBrand"));
/// assert_eq!(qr.nominal_power, Some((0, 11_000)));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ShipQr {
    /// The `secp256r1` Subject Key Identifier. Mandatory (`SRIP-310/4`).
    pub ski: Ski,
    /// The node's PIN, mandatory in the code when the node has one.
    pub pin: Option<String>,
    /// The SHIP ID. Mandatory (`SRIP-310/4`).
    pub id: Option<ShipId>,
    /// The brand shown to the installer.
    pub brand: Option<String>,
    /// The human-readable device type.
    pub device_type: Option<String>,
    /// The product model.
    pub model: Option<String>,
    /// The serial number.
    pub serial: Option<String>,
    /// The device categories.
    pub categories: Vec<DeviceCategory>,
    /// Nominal power as `(production, consumption)` in watts.
    ///
    /// Production is negative or zero and consumption positive or zero, so a
    /// feed-in-only device reads `(-8000, 0)` and a pure load `(0, 11000)`
    /// (`SRIP-310/12..14`).
    pub nominal_power: Option<(i64, i64)>,
    /// The `brainpoolP256r1` SKI, for nodes that support that curve (SHIP 1.1.0).
    pub brainpool_p256r1_ski: Option<Ski>,
    /// The `brainpoolP384r1` SKI, for nodes that support that curve (SHIP 1.1.0).
    pub brainpool_p384r1_ski: Option<Ski>,
    /// The `secp256r1` public key, hex encoded, offered instead of a certificate URL
    /// when stronger initial trust than a SKI is wanted.
    pub public_key: Option<String>,
    /// A URL the device's certificate can be downloaded from, in PEM form.
    pub certificate_url: Option<String>,
    /// The SHA-256 fingerprint of the DER certificate, used by the pairing service.
    pub certificate_fingerprint: Option<String>,
    /// The pairing secret (`SPSEC`) of the SHIP Pairing Service.
    ///
    /// This is key material, and so is [`pin`](Self::pin). Both are redacted by this
    /// type's [`Debug`], so `{:?}` on a scanned code is safe; [`ShipQr::redacted`] is for
    /// handing a sanitised *value* onward.
    pub pairing_secret: Option<String>,
    /// Keys this version does not know, kept in the order they appeared.
    pub unknown: Vec<(String, String)>,
}

/// Why a QR payload could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QrError {
    /// The payload did not start with the `SHIP;` marker.
    #[error("not a SHIP QR code: missing the `SHIP;` prefix")]
    NotShip,
    /// A field was not of the form `KEY:VALUE`.
    #[error("malformed field `{0}`: expected `KEY:VALUE`")]
    MalformedField(String),
    /// The mandatory `SKI` field was absent.
    #[error("mandatory field `SKI` is missing")]
    MissingSki,
    /// A field's value was not valid.
    #[error(transparent)]
    Value(#[from] DiscoveryError),
    /// The `SKI`, `BSKI` or `B2SKI` field did not hold a valid SKI.
    #[error("invalid SKI: {0}")]
    Ski(#[from] super::SkiParseError),
    /// `NOMINALPOWER` was not two comma-separated integers.
    #[error("invalid nominal power `{0}`: expected `<production>,<consumption>`")]
    NominalPower(String),
}

impl ShipQr {
    /// A QR payload with only the mandatory fields.
    pub fn new(ski: Ski, id: ShipId) -> Self {
        Self {
            ski,
            id: Some(id),
            ..Self::empty(ski)
        }
    }

    /// A payload with every optional field unset.
    ///
    /// Deliberately not a [`Default`] implementation: there is no sensible default
    /// [`Ski`], and a zero-filled one would look like an identity rather than the
    /// absence of one.
    fn empty(ski: Ski) -> Self {
        Self {
            ski,
            pin: None,
            id: None,
            brand: None,
            device_type: None,
            model: None,
            serial: None,
            categories: Vec::new(),
            nominal_power: None,
            brainpool_p256r1_ski: None,
            brainpool_p384r1_ski: None,
            public_key: None,
            certificate_url: None,
            certificate_fingerprint: None,
            pairing_secret: None,
            unknown: Vec::new(),
        }
    }

    /// Renders the payload in the order the specification prescribes.
    ///
    /// ```
    /// use eebus::ship::{ShipQr, ShipId};
    ///
    /// let qr = ShipQr::new(
    ///     "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap(),
    ///     ShipId::new("12345", "HEMS-1"),
    /// );
    /// assert_eq!(
    ///     qr.to_string(),
    ///     "SHIP;SKI:5555 AAAA FFFF 1111 CCCC 3333 EEEE DDDD 9999 2222;\
    ///      ID:i:12345_u:HEMS-1;ENDSHIP;"
    /// );
    /// ```
    pub fn to_payload(&self) -> String {
        let mut out = String::from("SHIP;");
        out.push_str("SKI:");
        out.push_str(&self.ski.to_display_string());
        out.push(';');

        if let Some(pin) = &self.pin {
            out.push_str("PIN:");
            out.push_str(&group_in_fours(pin));
            out.push(';');
        }
        if let Some(id) = &self.id {
            out.push_str("ID:");
            out.push_str(id.as_str());
            out.push(';');
        }
        for (key, value) in [
            ("BRAND", &self.brand),
            ("TYPE", &self.device_type),
            ("MODEL", &self.model),
        ] {
            if let Some(value) = value {
                out.push_str(key);
                out.push(':');
                out.push_str(value);
                out.push(';');
            }
        }
        if let Some(ski) = &self.brainpool_p256r1_ski {
            out.push_str("BSKI:");
            out.push_str(&ski.to_display_string());
            out.push(';');
        }
        if let Some(ski) = &self.brainpool_p384r1_ski {
            out.push_str("B2SKI:");
            out.push_str(&ski.to_display_string());
            out.push(';');
        }
        if let Some(key) = &self.public_key {
            out.push_str("PKEY:");
            out.push_str(key);
            out.push(';');
        }
        if let Some(url) = &self.certificate_url {
            out.push_str("CERTURL:");
            out.push_str(url);
            out.push(';');
        }
        if let Some(serial) = &self.serial {
            out.push_str("SERIAL:");
            out.push_str(serial);
            out.push(';');
        }
        if !self.categories.is_empty() {
            out.push_str("CAT:");
            out.push_str(
                &self
                    .categories
                    .iter()
                    .map(|c| c.number().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push(';');
        }
        if let Some((production, consumption)) = self.nominal_power {
            out.push_str("NOMINALPOWER:");
            out.push_str(&alloc::format!("{production},{consumption}"));
            out.push(';');
        }
        if let Some(fingerprint) = &self.certificate_fingerprint {
            out.push_str("FPH256:");
            out.push_str(fingerprint);
            out.push(';');
        }
        if let Some(secret) = &self.pairing_secret {
            out.push_str("SPSEC:");
            out.push_str(secret);
            out.push(';');
        }
        // Keys from a later version of the grammar go back out after the ones this
        // version knows: `SRIP-310/15` has a reader skip them rather than reject them,
        // which is only forward compatibility if a reader that re-emits keeps them.
        for (key, value) in &self.unknown {
            if !is_representable(key, value) {
                continue;
            }
            out.push_str(key);
            out.push(':');
            out.push_str(value);
            out.push(';');
        }
        out.push_str("ENDSHIP;");
        out
    }

    /// A copy with the pairing secret and PIN removed.
    ///
    /// [`Debug`] already redacts both, so this is for the other case: handing a scanned
    /// code onward — to an event, a diagnostic payload, a support bundle — with the key
    /// material taken out. The result is not a valid QR payload any more, and
    /// [`to_payload`](Self::to_payload) on it would produce a code that pairs with nothing.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            pin: self.pin.as_ref().map(|_| "<redacted>".to_owned()),
            pairing_secret: self
                .pairing_secret
                .as_ref()
                .map(|_| "<redacted>".to_owned()),
            ..self.clone()
        }
    }
}

/// Prints everything except the two fields that are key material.
///
/// A QR code is mostly public — that is the point of printing it on a sticker — but the
/// `PIN` and `SPSEC` fields are not, and a derived `Debug` would put both in the clear the
/// first time anybody logs a scan. Redacting here rather than in a method somebody has to
/// remember is what makes every value that carries a code safe to print.
impl fmt::Debug for ShipQr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const REDACTED: &str = "<redacted>";
        f.debug_struct("ShipQr")
            .field("ski", &self.ski)
            .field("pin", &self.pin.as_ref().map(|_| REDACTED))
            .field("id", &self.id)
            .field("brand", &self.brand)
            .field("device_type", &self.device_type)
            .field("model", &self.model)
            .field("serial", &self.serial)
            .field("categories", &self.categories)
            .field("nominal_power", &self.nominal_power)
            .field("brainpool_p256r1_ski", &self.brainpool_p256r1_ski)
            .field("brainpool_p384r1_ski", &self.brainpool_p384r1_ski)
            .field("public_key", &self.public_key)
            .field("certificate_url", &self.certificate_url)
            .field("certificate_fingerprint", &self.certificate_fingerprint)
            .field(
                "pairing_secret",
                &self.pairing_secret.as_ref().map(|_| REDACTED),
            )
            .field("unknown", &self.unknown)
            .finish()
    }
}

impl FromStr for ShipQr {
    type Err = QrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s.trim();
        let body = body.strip_prefix("SHIP;").ok_or(QrError::NotShip)?;

        // The SKI is mandatory and checked below; until it is parsed the placeholder
        // never escapes this function.
        let mut qr = ShipQr::empty(Ski::from_bytes([0; Ski::LEN]));
        let mut have_ski = false;

        for field in body.split(';') {
            let field = field.trim();
            if field.is_empty() || field == "ENDSHIP" {
                continue;
            }
            let (key, value) = field
                .split_once(':')
                .ok_or_else(|| QrError::MalformedField(field.to_owned()))?;
            let value = value.trim();

            match key {
                "SKI" => {
                    qr.ski = value.parse()?;
                    have_ski = true;
                }
                "BSKI" => qr.brainpool_p256r1_ski = Some(value.parse()?),
                "B2SKI" => qr.brainpool_p384r1_ski = Some(value.parse()?),
                // The ID is itself of the form `i:<pen>_u:<id>`, so it contains colons
                // and must be taken from the raw field rather than the split.
                "ID" => {
                    let raw = field.strip_prefix("ID:").unwrap_or(value);
                    qr.id = Some(raw.parse()?);
                }
                "PIN" => qr.pin = Some(strip_spaces(value)),
                "BRAND" => qr.brand = Some(value.to_owned()),
                "TYPE" => qr.device_type = Some(value.to_owned()),
                "MODEL" => qr.model = Some(value.to_owned()),
                "SERIAL" => qr.serial = Some(value.to_owned()),
                "PKEY" | "PBKEY" | "PB2KEY" => qr.public_key = Some(strip_spaces(value)),
                "CERTURL" | "CERTBURL" | "CERTB2URL" => {
                    let raw = field.split_once(':').map(|(_, v)| v).unwrap_or(value);
                    qr.certificate_url = Some(raw.trim().to_owned());
                }
                "FPH256" => qr.certificate_fingerprint = Some(strip_spaces(value)),
                "SPSEC" => qr.pairing_secret = Some(strip_spaces(value)),
                "CAT" => {
                    for part in value.split(',').filter(|p| !p.is_empty()) {
                        let n: u8 = part
                            .trim()
                            .parse()
                            .map_err(|_| DiscoveryError::Categories(value.to_owned()))?;
                        let cat = DeviceCategory::from_number(n)
                            .ok_or_else(|| DiscoveryError::Categories(value.to_owned()))?;
                        if !qr.categories.contains(&cat) {
                            qr.categories.push(cat);
                        }
                    }
                }
                "NOMINALPOWER" => {
                    let (production, consumption) = value
                        .split_once(',')
                        .ok_or_else(|| QrError::NominalPower(value.to_owned()))?;
                    let production = production
                        .trim()
                        .parse()
                        .map_err(|_| QrError::NominalPower(value.to_owned()))?;
                    let consumption = consumption
                        .trim()
                        .parse()
                        .map_err(|_| QrError::NominalPower(value.to_owned()))?;
                    qr.nominal_power = Some((production, consumption));
                }
                other => qr.unknown.push((other.to_owned(), value.to_owned())),
            }
        }

        if !have_ski {
            return Err(QrError::MissingSki);
        }
        Ok(qr)
    }
}

impl fmt::Display for ShipQr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_payload())
    }
}

/// The keys this version routes to a field of its own.
///
/// Only used to keep [`ShipQr::unknown`] from carrying one: the parser never puts a known
/// key there, but the field is public and an entry that shadows a known key would come
/// back as something else entirely.
const KNOWN_KEYS: &[&str] = &[
    "SKI",
    "BSKI",
    "B2SKI",
    "ID",
    "PIN",
    "BRAND",
    "TYPE",
    "MODEL",
    "SERIAL",
    "PKEY",
    "PBKEY",
    "PB2KEY",
    "CERTURL",
    "CERTBURL",
    "CERTB2URL",
    "FPH256",
    "SPSEC",
    "CAT",
    "NOMINALPOWER",
];

/// Whether `key:value` survives being written into a payload and read back.
///
/// The grammar has no escape, so a pair that would change meaning is dropped rather than
/// allowed to corrupt the fields around it: `;` ends a field and the first `:` ends a
/// key, and both ends of a field are trimmed on the way in. Everything the parser itself
/// produces satisfies this, which is what makes the round trip total.
fn is_representable(key: &str, value: &str) -> bool {
    !key.contains([';', ':'])
        && !value.contains(';')
        && !key.starts_with(char::is_whitespace)
        && !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
        && !KNOWN_KEYS.contains(&key)
}

/// Inserts a space every four characters, the form `SRIP-310/9` prescribes for the SKI
/// and the PIN so that a person can read them back.
fn group_in_fours(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + value.len() / 4);
    for (i, c) in value.chars().enumerate() {
        if i != 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn strip_spaces(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example of the installation requirements, §3.1.
    const EXAMPLE: &str = "SHIP;SKI:5555 AAAA FFFF 1111 CCCC 3333 EEEE DDDD 9999 2222;\
                           PIN:5555 AAAA FF;ID:i:12345_u:123abc456def;BRAND:ExampleBrand;\
                           TYPE:Heatpump;MODEL:E1234;SERIAL:123abc456def;CAT:4;\
                           NOMINALPOWER:0,11000;ENDSHIP;";

    const SKI: &str = "SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222";

    /// `SRIP-310/15`: a key from a later version of the grammar is skipped on the way in
    /// and kept on the way out. Dropping it would make a re-encoded code lossy.
    #[test]
    fn srip_310_15_unknown_keys_survive_a_round_trip() {
        let text = alloc::format!("SHIP;{SKI};NEWKEY:whatever;BRAND:Example;ENDSHIP;");
        let qr: ShipQr = text.parse().expect("parses");
        assert_eq!(qr.unknown, [("NEWKEY".to_owned(), "whatever".to_owned())]);

        let written = qr.to_payload();
        assert!(written.contains("NEWKEY:whatever;"), "{written}");
        assert_eq!(written.parse::<ShipQr>().expect("re-parses"), qr);
    }

    /// The shape `cargo fuzz` found: unknown values holding bytes that are neither
    /// printable nor whitespace, and a key that happens to end in a known one.
    #[test]
    fn unknown_values_may_hold_anything_the_grammar_permits() {
        let text = "SHIP;SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222;\
                    %:\u{0}\u{0}\n$;xxSKI:FFFF\n\nFF\u{0};ENDSHIP;";
        let qr: ShipQr = text.parse().expect("parses");
        assert_eq!(qr.unknown.len(), 2);
        // A leading newline is trimmed on the way in, so what is stored round trips.
        assert_eq!(qr.unknown[0], ("%".to_owned(), "\u{0}\u{0}\n$".to_owned()));
        assert_eq!(
            qr.unknown[1],
            ("xxSKI".to_owned(), "FFFF\n\nFF\u{0}".to_owned())
        );
        assert_eq!(qr.to_payload().parse::<ShipQr>().expect("re-parses"), qr);
    }

    /// A pair the grammar cannot express is dropped rather than left to corrupt the
    /// fields around it — there is no escape for `;` or for the `:` that ends a key.
    #[test]
    fn an_inexpressible_unknown_key_is_left_out() {
        let mut qr = ShipQr::new(
            "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap(),
            ShipId::new("12345", "HEMS-1"),
        );
        qr.unknown = alloc::vec![
            ("A;B".to_owned(), "x".to_owned()),
            ("A:B".to_owned(), "x".to_owned()),
            ("A".to_owned(), "x;y".to_owned()),
            (" A".to_owned(), "x".to_owned()),
            ("A".to_owned(), " x".to_owned()),
            ("A".to_owned(), "x ".to_owned()),
            // A known key would come back as that field rather than as an unknown one.
            ("BRAND".to_owned(), "x".to_owned()),
        ];
        let written = qr.to_payload();
        let back: ShipQr = written.parse().expect("re-parses");
        assert!(back.unknown.is_empty(), "{written}");
        assert_eq!(back.brand, None, "no key was smuggled into a known field");
        assert_eq!(back.ski, qr.ski);
    }

    /// [`KNOWN_KEYS`] is a second list of what the parser routes, so it can drift from
    /// the parser. Every entry has to be one the parser really does claim.
    #[test]
    fn every_known_key_is_one_the_parser_claims() {
        for key in KNOWN_KEYS {
            // A value each key accepts: the numeric ones are picky, the rest take text.
            let value = match *key {
                "CAT" => "4",
                "NOMINALPOWER" => "0,11000",
                "ID" => "i:12345_u:1",
                "SKI" | "BSKI" | "B2SKI" => "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222",
                _ => "x",
            };
            let text = alloc::format!("SHIP;{SKI};{key}:{value};ENDSHIP;");
            let qr: ShipQr = text.parse().unwrap_or_else(|e| panic!("{key}: {e}"));
            assert!(
                qr.unknown.is_empty(),
                "{key} is listed as known but the parser left it in `unknown`"
            );
        }
    }

    #[test]
    fn parses_the_specification_example() {
        let qr: ShipQr = EXAMPLE.parse().unwrap();
        assert_eq!(
            qr.ski.to_txt_value(),
            "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222"
        );
        assert_eq!(qr.pin.as_deref(), Some("5555AAAAFF"));
        assert_eq!(qr.id.as_ref().unwrap().as_str(), "i:12345_u:123abc456def");
        assert_eq!(qr.brand.as_deref(), Some("ExampleBrand"));
        assert_eq!(qr.device_type.as_deref(), Some("Heatpump"));
        assert_eq!(qr.model.as_deref(), Some("E1234"));
        assert_eq!(qr.serial.as_deref(), Some("123abc456def"));
        assert_eq!(qr.categories, vec![DeviceCategory::Hvac]);
        assert_eq!(qr.nominal_power, Some((0, 11_000)));
    }

    #[test]
    fn renders_back_to_the_specification_example() {
        let qr: ShipQr = EXAMPLE.parse().unwrap();
        assert_eq!(qr.to_payload(), EXAMPLE);
    }

    /// The SHIP Pairing Service Annex A.1 QR code for device A, which additionally
    /// carries the certificate fingerprint and the pairing secret.
    #[test]
    fn parses_the_pairing_service_example() {
        let text = "SHIP;SKI:A805 56A2 43F5 7CA1 7B0D 577A 59B7 11B7 9EB4 856C;\
                    ID:i:983327_u:C8277H008F-3;BRAND:EXAMPLEBRAND;TYPE:EMS;\
                    MODEL:EEB01devA814;SERIAL:C8277H008F-3;CAT:2;\
                    FPH256:C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943;\
                    SPSEC:7A37DCF81BDB50F8E92CFA4160CCB3DE;ENDSHIP;";
        let qr: ShipQr = text.parse().unwrap();

        assert_eq!(
            qr.ski.to_txt_value(),
            "A80556A243F57CA17B0D577A59B711B79EB4856C"
        );
        assert_eq!(qr.id.as_ref().unwrap().iana_pen(), Some("983327"));
        assert_eq!(
            qr.certificate_fingerprint.as_deref(),
            Some("C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943")
        );
        assert_eq!(
            qr.pairing_secret.as_deref(),
            Some("7A37DCF81BDB50F8E92CFA4160CCB3DE")
        );
        assert_eq!(qr.categories, vec![DeviceCategory::EnergyManagementSystem]);
    }

    /// `SRIP-310/15`: unknown keys are skipped, not rejected, so a code written to a
    /// later version of the specification still commissions this device.
    #[test]
    fn unknown_keys_are_kept_but_do_not_fail() {
        let text = "SHIP;SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222;\
                    ID:i:1_u:x;FUTUREKEY:whatever;ENDSHIP;";
        let qr: ShipQr = text.parse().unwrap();
        assert_eq!(
            qr.unknown,
            vec![("FUTUREKEY".to_string(), "whatever".to_string())]
        );
    }

    /// `SRIP-310/17`: a code without the closing marker is still valid.
    #[test]
    fn the_endship_marker_is_optional_on_input() {
        let text = "SHIP;SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222;ID:i:1_u:x;";
        let qr: ShipQr = text.parse().unwrap();
        assert_eq!(qr.id.unwrap().as_str(), "i:1_u:x");
    }

    #[test]
    fn a_payload_without_a_ski_is_rejected() {
        assert_eq!(
            "SHIP;ID:i:1_u:x;ENDSHIP;".parse::<ShipQr>(),
            Err(QrError::MissingSki)
        );
        assert_eq!("nonsense".parse::<ShipQr>(), Err(QrError::NotShip));
    }

    #[test]
    fn feed_in_only_devices_report_negative_nominal_power() {
        let text = "SHIP;SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222;ID:i:1_u:x;\
                    NOMINALPOWER:-8000,0;ENDSHIP;";
        let qr: ShipQr = text.parse().unwrap();
        assert_eq!(qr.nominal_power, Some((-8_000, 0)));
    }

    #[test]
    fn secrets_can_be_redacted_for_logging() {
        let qr: ShipQr = EXAMPLE.parse().unwrap();
        let safe = qr.redacted();
        assert_eq!(safe.pin.as_deref(), Some("<redacted>"));
        assert_eq!(safe.ski, qr.ski, "identity is not secret");
    }

    /// `{:?}` on a scanned code does not print the two fields that are key material.
    ///
    /// The sticker is public and so is nearly everything on it; the PIN and the pairing
    /// secret are the exceptions, and a derived `Debug` puts both in a log the first time
    /// anybody prints a scan. Redacting in a method somebody has to call is a rule, and
    /// this is a property.
    #[test]
    fn debug_does_not_print_the_pin_or_the_pairing_secret() {
        let text = "SHIP;SKI:5555AAAAFFFF1111CCCC3333EEEEDDDD99992222;ID:i:1_u:x;\
                    PIN:5555AAAAFF;SPSEC:0F1E2D3C4B5A69788796A5B4C3D2E1F0;ENDSHIP;";
        let qr: ShipQr = text.parse().expect("the example parses");
        assert_eq!(qr.pin.as_deref(), Some("5555AAAAFF"), "still readable");
        assert_eq!(
            qr.pairing_secret.as_deref(),
            Some("0F1E2D3C4B5A69788796A5B4C3D2E1F0")
        );

        let printed = alloc::format!("{qr:?}");
        assert!(
            !printed.contains("5555AAAAFF\""),
            "the PIN reached the log: {printed}"
        );
        assert!(
            !printed.contains("0F1E2D3C4B5A69788796A5B4C3D2E1F0"),
            "the pairing secret reached the log: {printed}"
        );
        assert_eq!(printed.matches("<redacted>").count(), 2);
        assert!(
            printed
                .to_ascii_uppercase()
                .contains("5555AAAAFFFF1111CCCC3333EEEEDDDD99992222"),
            "and the SKI, which is not secret, is still there: {printed}"
        );
    }
}
