//! SPINE addresses: naming a device, an entity and a feature.
//!
//! A SPINE resource is addressed in three parts (Protocol Specification §7.1.1). The
//! *device* part is a globally unique string; the *entity* part is a path, because
//! entities nest — a heat pump appliance may contain a compressor, which is where the
//! compressor's features live; and the *feature* part is a number within its entity.
//!
//! One address is fixed for every device: the primary NodeManagement instance sits on
//! entity `[0]`, feature `0`, and everything else is discovered through it.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{AddressDevice, AddressEntity, AddressFeature, FeatureAddress};

/// The entity that carries the primary NodeManagement instance.
pub const NODE_MANAGEMENT_ENTITY: u32 = 0;

/// The feature that carries the primary NodeManagement instance.
pub const NODE_MANAGEMENT_FEATURE: u32 = 0;

/// The longest a device address may be (Protocol Specification §7.1.1.2).
pub const MAX_DEVICE_ADDRESS_LEN: usize = 256;

/// Why a device address was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// The address did not begin with the `d:` marker.
    #[error("a device address begins with `d:`")]
    MissingPrefix,
    /// The vendor part was neither `i:<IANA PEN>` nor `n:<name>`.
    #[error("expected `i:<IANA PEN>` or `n:<vendor name>`, found `{0}`")]
    BadVendor(alloc::string::String),
    /// The unique part after the vendor was empty.
    #[error("the address has no vendor-unique part")]
    MissingUniquePart,
    /// The address contained a character the pattern excludes.
    #[error("the address contains the prohibited character {0:?}")]
    ProhibitedCharacter(char),
    /// The address was longer than 256 characters.
    #[error("a device address is limited to {MAX_DEVICE_ADDRESS_LEN} characters, found {0}")]
    TooLong(usize),
}

/// Builds a device address in the form the specification requires.
///
/// The pattern is `d:_(i:<IANA PEN>|n:<vendor name>)_<unique>`. An IANA Private
/// Enterprise Number is preferred — they are free and unique, where vendor names need a
/// registry nobody runs.
///
/// ```
/// use eebus::spine::device_address;
///
/// let address = device_address("i:46925", "HeatPump-0001").unwrap();
/// assert_eq!(address.as_str(), "d:_i:46925_HeatPump-0001");
/// ```
pub fn device_address(vendor: &str, unique: &str) -> Result<AddressDevice, AddressError> {
    let address = alloc::format!("d:_{vendor}_{unique}");
    validate_device_address(&address)?;
    Ok(AddressDevice::from(address))
}

/// Checks a device address against the pattern of §7.1.1.2.
///
/// Devices in the field are the ones sending these, so the check is used to reject a
/// peer's address rather than to be lenient about it: §7.1.1.5.1 requires terminating a
/// connection outright when two devices claim the same address.
pub fn validate_device_address(address: &str) -> Result<(), AddressError> {
    if address.chars().count() > MAX_DEVICE_ADDRESS_LEN {
        return Err(AddressError::TooLong(address.chars().count()));
    }
    let rest = address
        .strip_prefix("d:_")
        .ok_or(AddressError::MissingPrefix)?;

    let (vendor, unique) = split_vendor(rest)?;
    if unique.is_empty() {
        return Err(AddressError::MissingUniquePart);
    }
    for ch in address.chars() {
        // "Control", "Format" and "Separator" general categories are excluded; the
        // check below covers the ASCII cases, which is what devices actually send.
        if ch.is_control() || ch.is_whitespace() {
            return Err(AddressError::ProhibitedCharacter(ch));
        }
    }
    let _ = vendor;
    Ok(())
}

/// Splits `i:46925_HeatPump-1` into its vendor and unique parts.
fn split_vendor(rest: &str) -> Result<(&str, &str), AddressError> {
    let (marker, tail) = rest
        .split_at_checked(2)
        .ok_or_else(|| AddressError::BadVendor(rest.into()))?;
    let (vendor, unique) = match marker {
        "i:" => {
            let end = tail
                .find('_')
                .ok_or_else(|| AddressError::BadVendor(rest.into()))?;
            let (pen, unique) = tail.split_at(end);
            if pen.is_empty() || !pen.bytes().all(|b| b.is_ascii_digit()) || pen.starts_with('0') {
                return Err(AddressError::BadVendor(rest.into()));
            }
            (pen, &unique[1..])
        }
        "n:" => {
            let end = tail
                .find('_')
                .ok_or_else(|| AddressError::BadVendor(rest.into()))?;
            let (name, unique) = tail.split_at(end);
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(AddressError::BadVendor(rest.into()));
            }
            (name, &unique[1..])
        }
        _ => return Err(AddressError::BadVendor(rest.into())),
    };
    Ok((vendor, unique))
}

/// Builds a feature address.
///
/// The SPINE implementation guide §2.7 requires the device part in both the source and
/// the destination of every message, so it is not optional here — the one exception, the
/// very first detailed-discovery read, is served by
/// [`node_management_without_device`].
pub fn feature_address(device: &AddressDevice, entity: &[u32], feature: u32) -> FeatureAddress {
    FeatureAddress {
        device: Some(device.clone()),
        entity: Some(entity.iter().copied().map(AddressEntity).collect()),
        feature: Some(AddressFeature(feature)),
    }
}

/// The address of a device's primary NodeManagement instance.
pub fn node_management(device: &AddressDevice) -> FeatureAddress {
    feature_address(device, &[NODE_MANAGEMENT_ENTITY], NODE_MANAGEMENT_FEATURE)
}

/// The primary NodeManagement address without a device part.
///
/// The only message that may omit it: the first detailed-discovery read, when the
/// peer's device address is by definition not yet known (SPINE implementation guide
/// §2.7).
pub fn node_management_without_device() -> FeatureAddress {
    FeatureAddress {
        device: None,
        entity: Some(vec![AddressEntity(NODE_MANAGEMENT_ENTITY)]),
        feature: Some(AddressFeature(NODE_MANAGEMENT_FEATURE)),
    }
}

/// The entity path of a feature address, as plain numbers.
pub fn entity_path(address: &FeatureAddress) -> Vec<u32> {
    address.entity.iter().flatten().map(|e| e.get()).collect()
}

/// True when two addresses name the same feature.
///
/// The device part is compared only when both carry one, so that the first
/// detailed-discovery read still matches the NodeManagement instance it is aimed at.
pub fn same_feature(a: &FeatureAddress, b: &FeatureAddress) -> bool {
    if let (Some(x), Some(y)) = (&a.device, &b.device)
        && x != y
    {
        return false;
    }
    entity_path(a) == entity_path(b) && a.feature == b.feature
}

/// True when two addresses name features of the same entity.
///
/// The LPC implementation guide §3.8 turns this into a rule: an energy manager may
/// expose several `CEM` entities, and the one that binds `LoadControl` and
/// `DeviceConfiguration` must be one and the same — but which of its features it binds
/// from is its own business.
pub fn same_entity(a: &FeatureAddress, b: &FeatureAddress) -> bool {
    if let (Some(x), Some(y)) = (&a.device, &b.device)
        && x != y
    {
        return false;
    }
    entity_path(a) == entity_path(b)
}

/// True when the address names a primary NodeManagement instance.
pub fn is_node_management(address: &FeatureAddress) -> bool {
    entity_path(address) == [NODE_MANAGEMENT_ENTITY]
        && address.feature == Some(AddressFeature(NODE_MANAGEMENT_FEATURE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specifications_example_addresses_are_valid() {
        // Protocol Specification §7.1.1.2.
        for address in ["d:_i:46925_ABCabc-123", "d:_i:46925_0123456789"] {
            assert_eq!(validate_device_address(address), Ok(()), "{address}");
        }
        assert_eq!(
            validate_device_address("d:_n:ExampleBrand_Device-1"),
            Ok(())
        );
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        assert_eq!(
            validate_device_address("i:46925_x"),
            Err(AddressError::MissingPrefix)
        );
        assert_eq!(
            validate_device_address("d:_x:46925_y"),
            Err(AddressError::BadVendor("x:46925_y".into()))
        );
        assert_eq!(
            validate_device_address("d:_i:046925_y"),
            Err(AddressError::BadVendor("i:046925_y".into())),
            "an IANA PEN does not start with zero"
        );
        assert_eq!(
            validate_device_address("d:_i:46925_"),
            Err(AddressError::MissingUniquePart)
        );
        assert_eq!(
            validate_device_address("d:_i:46925_a b"),
            Err(AddressError::ProhibitedCharacter(' '))
        );
        assert!(matches!(
            validate_device_address(&alloc::format!("d:_i:1_{}", "x".repeat(300))),
            Err(AddressError::TooLong(_))
        ));
    }

    #[test]
    fn node_management_sits_at_entity_zero_feature_zero() {
        let device = device_address("i:46925", "Device-1").unwrap();
        let address = node_management(&device);
        assert_eq!(entity_path(&address), [0]);
        assert_eq!(address.feature, Some(AddressFeature(0)));
        assert!(is_node_management(&address));
        assert!(is_node_management(&node_management_without_device()));
    }

    #[test]
    fn addresses_match_across_a_missing_device_part() {
        let device = device_address("i:46925", "Device-1").unwrap();
        assert!(same_feature(
            &node_management(&device),
            &node_management_without_device()
        ));

        let other = device_address("i:46925", "Device-2").unwrap();
        assert!(
            !same_feature(&node_management(&device), &node_management(&other)),
            "two devices are not the same feature"
        );
    }

    #[test]
    fn nested_entities_keep_their_path() {
        let device = device_address("i:46925", "HeatPump").unwrap();
        let compressor = feature_address(&device, &[1, 2], 3);
        assert_eq!(entity_path(&compressor), [1, 2]);
        assert!(!is_node_management(&compressor));
    }
}
