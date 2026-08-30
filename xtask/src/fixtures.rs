//! Turning the specification's example XMLs into wire-format test fixtures.
//!
//! `EEBus_SPINE_V1.3.0_Final.zip` ships thirty Restricted Function Exchange examples as
//! XML, one per combination of operation, command control and filter shape. Converting
//! them with the schema in hand yields exactly the JSON a conforming peer would put on
//! the wire, which makes them the best available ground truth for the codec: a round
//! trip through the Rust model has to reproduce them byte for byte.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::xsd::{self, ComplexDef, Particle, Schema, SimpleDef};

pub fn run(root: &Path) -> Result<String, String> {
    let base = root.join("specs/EEBus_SPINE_V1.3.0/EEBus_SPINE_V1.3.0_Final_hp");
    let xsd_dir = base.join("XSDs");
    let xml_dir = base.join("ExampleXMLs/RestrictedFunctionExchange");
    if !xml_dir.is_dir() {
        return Err(format!(
            "SPINE example XMLs not found at {}; unpack EEBus_SPINE_V1.3.0.zip into specs/",
            xml_dir.display()
        ));
    }

    let schema = xsd::load_dir(&xsd_dir, crate::naming::module_name)?;
    let out_dir = root.join("tests/fixtures/spine");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("creating {out_dir:?}: {e}"))?;

    let mut files: Vec<_> = std::fs::read_dir(&xml_dir)
        .map_err(|e| format!("reading {}: {e}", xml_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "xml"))
        .collect();
    files.sort();

    let mut count = 0usize;
    for path in &files {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc =
            roxmltree::Document::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let root_el = doc.root_element();
        let name = root_el.tag_name().name().to_string();
        let ty = schema
            .elements
            .get(&name)
            .ok_or_else(|| format!("{}: unknown root element `{name}`", path.display()))?;

        let mut json = String::new();
        write!(json, "{{{:?}:", name).unwrap();
        convert(&schema, &root_el, ty, &mut json)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        json.push('}');

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("example")
            .trim_start_matches("EEBus_SPINE_Spec_Example_");
        let out = out_dir.join(format!("{stem}.json"));
        std::fs::write(&out, format!("{json}\n")).map_err(|e| format!("writing {out:?}: {e}"))?;
        count += 1;
    }

    Ok(format!(
        "wrote {count} SPINE example fixtures to {}",
        out_dir.display()
    ))
}

/// Writes the JSON representation of `node`, whose XSD type is `ty`.
fn convert(
    schema: &Schema,
    node: &roxmltree::Node<'_, '_>,
    ty: &str,
    out: &mut String,
) -> Result<(), String> {
    if ty == "ElementTagType" {
        out.push_str("[]");
        return Ok(());
    }

    if let Some(def) = schema.complex.get(ty) {
        let fields = field_types(schema, ty, def, 0)?;
        out.push('[');
        let mut first = true;
        let mut emitted: Vec<&str> = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            let wire = child.tag_name().name();
            if emitted.contains(&wire) {
                continue;
            }
            let (child_ty, repeated) = fields
                .get(wire)
                .cloned()
                .ok_or_else(|| format!("`{ty}` has no element `{wire}`"))?;

            if !first {
                out.push(',');
            }
            first = false;
            write!(out, "{{{wire:?}:").unwrap();

            if repeated {
                emitted.push(wire);
                out.push('[');
                let mut sep = false;
                for sib in node
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == wire)
                {
                    if sep {
                        out.push(',');
                    }
                    sep = true;
                    convert(schema, &sib, &child_ty, out)?;
                }
                out.push(']');
            } else {
                convert(schema, &child, &child_ty, out)?;
            }
            out.push('}');
        }
        out.push(']');
        return Ok(());
    }

    // A simple type: emit the lexical value with the JSON type the schema implies.
    let text = node.text().unwrap_or("").trim();
    match json_kind(schema, ty, 0) {
        JsonKind::Number => out.push_str(text),
        JsonKind::Bool => out.push_str(if text == "true" || text == "1" {
            "true"
        } else {
            "false"
        }),
        JsonKind::String => write!(out, "{text:?}").unwrap(),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum JsonKind {
    Number,
    Bool,
    String,
}

fn json_kind(schema: &Schema, ty: &str, depth: usize) -> JsonKind {
    if depth > 8 {
        return JsonKind::String;
    }
    match ty {
        "xs:boolean" => return JsonKind::Bool,
        t if t.starts_with("xs:") => {
            return match t {
                "xs:byte" | "xs:short" | "xs:int" | "xs:long" | "xs:integer"
                | "xs:unsignedByte" | "xs:unsignedShort" | "xs:unsignedInt" | "xs:unsignedLong"
                | "xs:float" | "xs:double" | "xs:decimal" => JsonKind::Number,
                _ => JsonKind::String,
            };
        }
        _ => {}
    }
    match schema.simple.get(ty) {
        Some(SimpleDef::Restrict { base }) => json_kind(schema, base, depth + 1),
        Some(SimpleDef::Enum { .. }) | Some(SimpleDef::Union { .. }) => JsonKind::String,
        _ => JsonKind::String,
    }
}

/// Wire name → (type, repeated) for every element a complex type may contain,
/// including those contributed by base types and by referenced groups.
fn field_types(
    schema: &Schema,
    owner: &str,
    def: &ComplexDef,
    depth: usize,
) -> Result<BTreeMap<String, (String, bool)>, String> {
    if depth > 8 {
        return Err(format!("type hierarchy of {owner} is too deep"));
    }
    let mut out = BTreeMap::new();

    if let Some(base) = &def.extends
        && let Some(base_def) = schema.complex.get(base)
    {
        out.extend(field_types(schema, base, base_def, depth + 1)?);
    }
    if let Some(base) = &def.restricts
        && let Some(base_def) = schema.complex.get(base)
    {
        out.extend(field_types(schema, base, base_def, depth + 1)?);
    }

    for p in &def.particles {
        match p {
            Particle::Element { wire, ty, repeated } => {
                let resolved = match ty.strip_prefix("@element:") {
                    Some(el) => schema
                        .elements
                        .get(el)
                        .cloned()
                        .ok_or_else(|| format!("undeclared element `{el}`"))?,
                    None => ty.clone(),
                };
                out.insert(wire.clone(), (resolved, *repeated));
            }
            Particle::Group { name, repeated } => {
                let group = schema
                    .groups
                    .get(name)
                    .ok_or_else(|| format!("{owner} references unknown group {name}"))?;
                let inline = ComplexDef {
                    extends: None,
                    restricts: None,
                    simple_content: None,
                    particles: group.particles.clone(),
                };
                let mut inner = field_types(schema, name, &inline, depth + 1)?;
                if *repeated {
                    for v in inner.values_mut() {
                        // A repeated choice group still contributes one element per
                        // occurrence, so its members stay non-repeating.
                        v.1 = false;
                    }
                }
                out.extend(inner);
            }
        }
    }
    Ok(out)
}
