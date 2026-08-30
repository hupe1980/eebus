//! Translating XSD names into Rust names.

/// Rust type name for an XSD type: `LoadControlLimitDataType` → `LoadControlLimitData`.
pub fn type_name(xsd: &str) -> String {
    let base = xsd.strip_suffix("Type").unwrap_or(xsd);
    if base.is_empty() {
        return xsd.to_string();
    }
    pascal(base)
}

/// Rust enum variant for a wire value: `signDependentAbsValueLimit` → `SignDependentAbsValueLimit`.
///
/// SPINE's unit-of-measurement enumeration uses symbols that are not identifiers —
/// `l/s`, `m^3`, `Imp.gal`, `J/kg_K` — so operators become words: `/` reads as `Per`
/// and `^` as `Pow`, giving `LPerS`, `MPow3`, `ImpGal` and `JPerKgK`.
pub fn variant_name(wire: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, words: &mut Vec<String>| {
        if !current.is_empty() {
            words.push(core::mem::take(current));
        }
    };
    for c in wire.chars() {
        match c {
            '/' => {
                flush(&mut current, &mut words);
                words.push("Per".to_string());
            }
            '^' => {
                flush(&mut current, &mut words);
                words.push("Pow".to_string());
            }
            '%' => {
                flush(&mut current, &mut words);
                words.push("Percent".to_string());
            }
            '°' => {
                flush(&mut current, &mut words);
                words.push("Deg".to_string());
            }
            '*' | 'ⅹ' => {
                flush(&mut current, &mut words);
                words.push("Times".to_string());
            }
            '.' | '_' | '-' | ' ' | ':' => flush(&mut current, &mut words),
            c if c.is_ascii_alphanumeric() => current.push(c),
            _ => {}
        }
    }
    flush(&mut current, &mut words);

    let joined: String = words
        .iter()
        .map(|w| {
            let mut it = w.chars();
            match it.next() {
                Some(first) => first.to_uppercase().collect::<String>() + it.as_str(),
                None => String::new(),
            }
        })
        .collect();

    if joined.is_empty() {
        "Empty".to_string()
    } else if joined.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("V{joined}")
    } else {
        joined
    }
}

/// Rust field name for an XSD element: `isLimitActive` → `is_limit_active`.
pub fn field_name(wire: &str) -> String {
    let s = snake(wire);
    if is_keyword(&s) { format!("{s}_") } else { s }
}

/// Rust module name for a schema file stem: `EEBus_SPINE_TS_LoadControl` → `load_control`.
pub fn module_name(stem: &str) -> String {
    let tail = stem.rsplit("_TS_").next().unwrap_or(stem);
    snake(tail)
}

pub fn pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `camelCase`/`PascalCase`/acronym-aware conversion to `snake_case`.
///
/// Runs of capitals are kept together so `HVACSystemFunction` becomes
/// `hvac_system_function` rather than `h_v_a_c_system_function`.
pub fn snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            continue;
        }
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                None => false,
                Some(p) if p == '_' || p == '-' => false,
                // lower/digit → upper: `limitId` ⇒ `limit_id`
                Some(p) if p.is_ascii_lowercase() => true,
                // upper → upper followed by lower: `HVACRoom` ⇒ `hvac_room`
                Some(p) if p.is_ascii_uppercase() && c.is_ascii_uppercase() => {
                    next.is_some_and(|n| n.is_ascii_lowercase())
                }
                _ => false,
            };
            if boundary && !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn is_keyword(s: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
        "gen",
    ];
    KEYWORDS.contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_handles_acronyms_and_digits() {
        assert_eq!(snake("isLimitActive"), "is_limit_active");
        assert_eq!(snake("HVACRoom"), "hvac_room");
        assert_eq!(snake("hvacOperationModeType"), "hvac_operation_mode_type");
        assert_eq!(snake("scaledNumber"), "scaled_number");
        assert_eq!(snake("dnsSd_mDns"), "dns_sd_m_dns");
        assert_eq!(snake("evseManufacturerData"), "evse_manufacturer_data");
    }

    #[test]
    fn type_names_drop_the_type_suffix() {
        assert_eq!(
            type_name("LoadControlLimitDataType"),
            "LoadControlLimitData"
        );
        assert_eq!(type_name("FunctionType"), "Function");
        assert_eq!(
            type_name("LoadControlLimitTypeType"),
            "LoadControlLimitType"
        );
    }

    #[test]
    fn keywords_are_escaped() {
        assert_eq!(field_name("type"), "type_");
        assert_eq!(field_name("ref"), "ref_");
    }

    #[test]
    fn module_names_come_from_the_schema_stem() {
        assert_eq!(module_name("EEBus_SPINE_TS_LoadControl"), "load_control");
        assert_eq!(module_name("EEBus_SPINE_TS_HVAC"), "hvac");
    }
}

#[cfg(test)]
mod unit_tests {
    use super::variant_name;

    #[test]
    fn unit_symbols_become_identifiers() {
        assert_eq!(variant_name("l/s"), "LPerS");
        assert_eq!(variant_name("m^3"), "MPow3");
        assert_eq!(variant_name("Imp.gal"), "ImpGal");
        assert_eq!(variant_name("J/kg_K"), "JPerKgK");
        assert_eq!(variant_name("US.liq.gal/h"), "USLiqGalPerH");
        assert_eq!(variant_name("1"), "V1");
        assert_eq!(variant_name("Bq/m^3"), "BqPerMPow3");
    }
}
