//! Lowering the parsed EEBUS schemas to Rust.
//!
//! Two schemas go through here: SPINE 1.3.0, whose forty files become one module each
//! under `src/model/generated/`, and the SHIP 1.1.0 transfer protocol, which is small
//! enough to be a single module under `src/ship/generated.rs`. Both use the same
//! XML→JSON mapping, so both use the same emitters.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::naming::{field_name, module_name, pascal, snake, type_name, variant_name};
use crate::xsd::{self, ComplexDef, GroupDef, Particle, Schema, SimpleDef};

/// How one schema is turned into Rust.
struct Profile {
    /// Human-readable name, used in the report and in module docs.
    label: &'static str,
    /// Specification version, for the `@generated` banner.
    version: &'static str,
    xsd_dir: PathBuf,
    out_dir: PathBuf,
    /// `Some(name)` collapses the whole schema into one module file.
    single_module: Option<&'static str>,
    /// Types written by hand instead of generated.
    handwritten: &'static [&'static str],
    /// Groups that are not lowered at all.
    skip_groups: &'static [&'static str],
    /// `(group, enum name, field name)` for choice groups.
    choices: &'static [(&'static str, &'static str, &'static str)],
    /// Groups whose alternatives are folded into the enclosing choice.
    inline_choices: &'static [&'static str],
    /// The module path prefix generated modules import from.
    parent_use: &'static str,
}

const SPINE_CHOICES: &[(&str, &str, &str)] = &[
    ("DataChoiceGroup", "CmdData", "data"),
    ("DataSelectorsChoiceGroup", "FilterSelectors", "selectors"),
    ("DataElementsChoiceGroup", "FilterElements", "elements"),
];

const SHIP_CHOICES: &[(&str, &str, &str)] = &[
    ("MsgTypeControlGroup", "ControlMessage", "control"),
    ("MsgTypeDataGroup", "DataMessage", "data"),
    ("MsgTypeEndGroup", "EndMessage", "end"),
];

pub fn run(root: &Path) -> Result<String, String> {
    let spine = Profile {
        label: "SPINE",
        version: "1.3.0",
        xsd_dir: root.join("specs/EEBus_SPINE_V1.3.0/EEBus_SPINE_V1.3.0_Final_hp/XSDs"),
        out_dir: root.join("src/model/generated"),
        single_module: None,
        handwritten: &["ElementTagType", "CmdType", "FilterType", "ErrorNumberType"],
        skip_groups: &[],
        choices: SPINE_CHOICES,
        inline_choices: &[],
        parent_use: "super::super",
    };
    let ship = Profile {
        label: "SHIP",
        version: "1.1.0",
        xsd_dir: root.join(
            "specs/EEBus_SHIP_TS_Specification_v1.1.0_public/\
             EEBus_SHIP_TS_Specification_v1.1.0_public",
        ),
        out_dir: root.join("src/ship/generated"),
        single_module: Some("messages"),
        handwritten: &[],
        // MSG_TYPE=init is the two-byte CMI handshake, not a JSON message, and
        // MessageValueGroup only exists to union the four message-type groups.
        skip_groups: &["MsgTypeInitGroup", "MessageValueGroup"],
        choices: SHIP_CHOICES,
        inline_choices: &["MsgTypeControlKeyMaterialExchangeGroup"],
        parent_use: "super::super",
    };

    let mut report = String::new();
    for profile in [spine, ship] {
        if !profile.xsd_dir.is_dir() {
            return Err(format!(
                "{} schemas not found at {}.\n\
                 Download them from https://www.eebus.org/specifications-media/ \
                 (free registration) and unpack into specs/.",
                profile.label,
                profile.xsd_dir.display()
            ));
        }
        let schema = xsd::load_dir(&profile.xsd_dir, |stem| {
            profile
                .single_module
                .map_or_else(|| module_name(stem), str::to_string)
        })?;
        let lowered = lower(&profile, &schema)?;
        write_modules(&profile, &lowered)?;
        writeln!(
            report,
            "{}: {} types ({} structs, {} enums, {} newtypes, {} choices, {} aliases) \
             in {} module(s) → {}",
            profile.label,
            lowered.total(),
            lowered.structs,
            lowered.enums,
            lowered.newtypes,
            lowered.choices,
            lowered.aliases,
            lowered.modules.len(),
            profile.out_dir.display()
        )
        .unwrap();
    }
    Ok(report.trim_end().to_string())
}

#[derive(Default)]
struct Lowered {
    modules: BTreeMap<String, String>,
    structs: usize,
    enums: usize,
    newtypes: usize,
    choices: usize,
    aliases: usize,
}

impl Lowered {
    fn total(&self) -> usize {
        self.structs + self.enums + self.newtypes + self.choices + self.aliases
    }
}

struct Ctx<'a> {
    profile: &'a Profile,
    schema: &'a Schema,
    /// `*EnumType` names absorbed into an extensible enum.
    consumed: BTreeSet<String>,
}

impl Ctx<'_> {
    fn choice_type(&self, group: &str) -> String {
        self.profile
            .choices
            .iter()
            .find(|(g, _, _)| *g == group)
            .map(|(_, t, _)| (*t).to_string())
            .unwrap_or_else(|| pascal(group.strip_suffix("Group").unwrap_or(group)))
    }

    fn choice_field(&self, group: &str) -> String {
        self.profile
            .choices
            .iter()
            .find(|(g, _, _)| *g == group)
            .map(|(_, _, f)| (*f).to_string())
            .unwrap_or_else(|| snake(group.strip_suffix("Group").unwrap_or(group)))
    }

    fn is_generated_choice(&self, group: &str) -> bool {
        !self.profile.skip_groups.contains(&group) && !self.profile.inline_choices.contains(&group)
    }
}

fn lower(profile: &Profile, schema: &Schema) -> Result<Lowered, String> {
    let mut consumed = BTreeSet::new();
    for def in schema.simple.values() {
        if let SimpleDef::Union { members } = def
            && members.iter().any(|m| m == "EnumExtendType")
        {
            for m in members {
                if m.ends_with("EnumType") {
                    consumed.insert(m.clone());
                }
            }
        }
    }
    let ctx = Ctx {
        profile,
        schema,
        consumed,
    };

    let mut out = Lowered::default();
    let mut bodies: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let default_module = profile.single_module.unwrap_or("common_data_types");

    let module_of = |name: &str| {
        schema
            .module_of
            .get(name)
            .cloned()
            .unwrap_or_else(|| default_module.to_string())
    };

    for (name, def) in &schema.simple {
        if ctx.consumed.contains(name) || profile.handwritten.contains(&name.as_str()) {
            continue;
        }
        let (text, kind) = emit_simple(&ctx, name, def)?;
        out.count(&kind);
        bodies
            .entry(module_of(name))
            .or_default()
            .push((name.clone(), text));
    }

    for (name, def) in &schema.complex {
        if profile.handwritten.contains(&name.as_str()) {
            continue;
        }
        let (text, kind) = emit_complex(&ctx, name, def)?;
        out.count(&kind);
        bodies
            .entry(module_of(name))
            .or_default()
            .push((name.clone(), text));
    }

    for (name, group) in &schema.groups {
        if !group.is_choice || !ctx.is_generated_choice(name) {
            continue;
        }
        let text = emit_choice(&ctx, name, group)?;
        out.choices += 1;
        let module = profile.single_module.unwrap_or("command_frame").to_string();
        bodies.entry(module).or_default().push((name.clone(), text));
    }

    emit_rfe_metadata(&ctx, &mut bodies, &module_of)?;

    for (module, mut items) in bodies {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let mut src = module_header(profile, &module);
        for (_, text) in items {
            src.push_str(&text);
            src.push('\n');
        }
        out.modules.insert(module, src);
    }

    Ok(out)
}

impl Lowered {
    fn count(&mut self, kind: &Kind) {
        match kind {
            Kind::Struct => self.structs += 1,
            Kind::Enum => self.enums += 1,
            Kind::Newtype => self.newtypes += 1,
            Kind::Alias => self.aliases += 1,
        }
    }
}

enum Kind {
    Struct,
    Enum,
    Newtype,
    Alias,
}

fn module_header(profile: &Profile, module: &str) -> String {
    format!(
        "//! {label} `{module}` types.\n\
         //!\n\
         //! @generated by `cargo xtask codegen` from the {label} {version} XML Schema.\n\
         //! Do not edit by hand; edit the generator instead.\n\
         \n\
         #[allow(unused_imports)]\n\
         use {parent}::*;\n\
         #[allow(unused_imports)]\n\
         use alloc::{{string::String, vec::Vec}};\n\n",
        label = profile.label,
        version = profile.version,
        parent = profile.parent_use,
    )
}

fn emit_simple(ctx: &Ctx<'_>, name: &str, def: &SimpleDef) -> Result<(String, Kind), String> {
    let rust = type_name(name);
    let mut s = String::new();
    match def {
        SimpleDef::Enum { values } => {
            writeln!(s, "crate::eebus_enum! {{").unwrap();
            writeln!(s, "    /// `{name}`.").unwrap();
            writeln!(s, "    pub enum {rust} {{").unwrap();
            for (wire, variant) in dedupe_variants(values) {
                writeln!(s, "        /// `{wire}`.").unwrap();
                writeln!(s, "        {wire:?} => {variant},").unwrap();
            }
            writeln!(s, "    }}\n}}").unwrap();
            Ok((s, Kind::Enum))
        }
        SimpleDef::Union { members } => {
            if members.iter().any(|m| m == "EnumExtendType")
                && let Some(em) = members.iter().find(|m| m.ends_with("EnumType"))
                && let Some(SimpleDef::Enum { values }) = ctx.schema.simple.get(em)
            {
                writeln!(s, "crate::eebus_enum_ext! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    ///").unwrap();
                writeln!(
                    s,
                    "    /// Extensible: vendors may add values of the form `_i:<PEN>_<name>`,"
                )
                .unwrap();
                writeln!(s, "    /// which are preserved in [`Other`](Self::Other).").unwrap();
                writeln!(s, "    pub enum {rust} {{").unwrap();
                for (wire, variant) in dedupe_variants(values) {
                    writeln!(s, "        /// `{wire}`.").unwrap();
                    writeln!(s, "        {wire:?} => {variant},").unwrap();
                }
                writeln!(s, "    }}\n}}").unwrap();
                return Ok((s, Kind::Enum));
            }
            writeln!(s, "crate::eebus_str! {{").unwrap();
            writeln!(s, "    /// `{name}` (union of `{}`).", members.join(" | ")).unwrap();
            writeln!(s, "    pub struct {rust};\n}}").unwrap();
            Ok((s, Kind::Newtype))
        }
        SimpleDef::List { item } => {
            let inner = rust_type(ctx, item, false)?;
            writeln!(s, "/// `{name}` (whitespace-separated list).").unwrap();
            writeln!(s, "pub type {rust} = Vec<{inner}>;").unwrap();
            Ok((s, Kind::Alias))
        }
        SimpleDef::Restrict { base } => match builtin(base) {
            Some("String") => {
                writeln!(s, "crate::eebus_str! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    pub struct {rust};\n}}").unwrap();
                Ok((s, Kind::Newtype))
            }
            Some(prim) if prim == "bool" || prim.starts_with('f') => {
                writeln!(s, "/// `{name}`.").unwrap();
                writeln!(s, "pub type {rust} = {prim};").unwrap();
                Ok((s, Kind::Alias))
            }
            Some(prim) => {
                writeln!(s, "crate::eebus_id! {{").unwrap();
                writeln!(s, "    /// `{name}`.").unwrap();
                writeln!(s, "    pub struct {rust}({prim});\n}}").unwrap();
                Ok((s, Kind::Newtype))
            }
            None => {
                let target = rust_type(ctx, base, false)?;
                writeln!(s, "/// `{name}`.").unwrap();
                writeln!(s, "pub type {rust} = {target};").unwrap();
                Ok((s, Kind::Alias))
            }
        },
    }
}

fn emit_complex(ctx: &Ctx<'_>, name: &str, def: &ComplexDef) -> Result<(String, Kind), String> {
    let rust = type_name(name);

    // `xs:simpleContent` means the type carries a scalar, with attributes the JSON
    // mapping absorbs; it is that scalar as far as the wire is concerned.
    if let Some(base) = &def.simple_content {
        let target = rust_type(ctx, base, false)?;
        let mut s = String::new();
        writeln!(s, "/// `{name}` (a `{base}` with attributes).").unwrap();
        writeln!(s, "pub type {rust} = {target};").unwrap();
        return Ok((s, Kind::Alias));
    }

    let fields = flatten_fields(ctx, name, def, 0)?;

    if fields.is_empty()
        && let Some(base) = &def.restricts
    {
        let target = type_name(base);
        let mut s = String::new();
        writeln!(s, "/// `{name}` (restriction of `{base}`).").unwrap();
        writeln!(s, "pub type {rust} = {target};").unwrap();
        return Ok((s, Kind::Alias));
    }

    let mut s = String::new();
    writeln!(s, "crate::eebus_struct! {{").unwrap();
    writeln!(s, "    /// `{name}`.").unwrap();
    writeln!(s, "    pub struct {rust} {{").unwrap();
    for f in &fields {
        writeln!(s, "        /// `{}`.", f.doc_name()).unwrap();
        writeln!(
            s,
            "        {:?} => {}: {},",
            f.wire, f.rust_name, f.rust_type
        )
        .unwrap();
    }
    writeln!(s, "    }}\n}}").unwrap();
    Ok((s, Kind::Struct))
}

fn emit_choice(ctx: &Ctx<'_>, group: &str, def: &GroupDef) -> Result<String, String> {
    let rust = ctx.choice_type(group);
    let mut alternatives = Vec::new();
    collect_alternatives(ctx, group, def, &mut alternatives, 0)?;

    let mut s = String::new();
    writeln!(s, "crate::eebus_choice! {{").unwrap();
    writeln!(s, "    /// `{group}`: exactly one alternative.").unwrap();
    writeln!(s, "    pub enum {rust} {{").unwrap();
    let mut seen = BTreeSet::new();
    for (wire, ty) in alternatives {
        let variant = variant_name(&wire);
        if !seen.insert(variant.clone()) {
            continue;
        }
        writeln!(s, "        /// `{wire}`.").unwrap();
        writeln!(s, "        {wire:?} => {variant}({ty}),").unwrap();
    }
    writeln!(s, "    }}\n}}").unwrap();
    Ok(s)
}

/// Gathers a choice group's alternatives, folding in nested groups listed as inline.
fn collect_alternatives(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &GroupDef,
    out: &mut Vec<(String, String)>,
    depth: usize,
) -> Result<(), String> {
    if depth > 4 {
        return Err(format!("choice group {owner} nests too deeply"));
    }
    for p in &def.particles {
        match p {
            Particle::Element { wire, ty, .. } => {
                let resolved = resolve(ctx, ty)?;
                out.push((wire.clone(), rust_type(ctx, &resolved, false)?));
            }
            Particle::Group { name, .. } => {
                let nested = ctx
                    .schema
                    .groups
                    .get(name)
                    .ok_or_else(|| format!("{owner} references unknown group {name}"))?;
                collect_alternatives(ctx, name, nested, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

struct Field {
    wire: String,
    rust_name: String,
    rust_type: String,
    /// The resolved XSD type, needed to pair a data type with its elements filter.
    xsd_type: String,
}

impl Field {
    /// The name to show in documentation; choice groups have no wire name of their own.
    fn doc_name(&self) -> &str {
        self.wire
            .strip_prefix("@choice:")
            .unwrap_or(self.wire.as_str())
    }
}

fn flatten_fields(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &ComplexDef,
    depth: usize,
) -> Result<Vec<Field>, String> {
    if depth > 8 {
        return Err(format!("type hierarchy of {owner} is too deep"));
    }
    let mut out = Vec::new();

    if let Some(base) = &def.extends
        && let Some(base_def) = ctx.schema.complex.get(base)
    {
        out.extend(flatten_fields(ctx, base, base_def, depth + 1)?);
    }

    for p in &def.particles {
        match p {
            Particle::Element { wire, ty, repeated } => {
                let resolved = resolve(ctx, ty)?;
                let rust_type = rust_type(ctx, &resolved, *repeated)?;
                out.push(Field {
                    wire: wire.clone(),
                    rust_name: field_name(wire),
                    rust_type,
                    xsd_type: resolved,
                });
            }
            Particle::Group { name, repeated } => {
                let group = ctx
                    .schema
                    .groups
                    .get(name)
                    .ok_or_else(|| format!("{owner} references unknown group {name}"))?;
                if group.is_choice {
                    let ty = ctx.choice_type(name);
                    let ty = if *repeated { format!("Vec<{ty}>") } else { ty };
                    out.push(Field {
                        wire: format!("@choice:{name}"),
                        rust_name: ctx.choice_field(name),
                        rust_type: ty,
                        xsd_type: name.clone(),
                    });
                } else {
                    let inline = ComplexDef {
                        extends: None,
                        restricts: None,
                        simple_content: None,
                        particles: group.particles.clone(),
                    };
                    out.extend(flatten_fields(ctx, name, &inline, depth + 1)?);
                }
            }
        }
    }

    // The schemas occasionally repeat an inherited element in a derived type; the last
    // declaration wins, matching XSD restriction semantics.
    let mut seen = BTreeSet::new();
    let mut deduped: Vec<Field> = Vec::new();
    for f in out.into_iter().rev() {
        if seen.insert(f.rust_name.clone()) {
            deduped.push(f);
        }
    }
    deduped.reverse();
    Ok(deduped)
}

/// Resolves `@element:name` indirections to the element's type name.
fn resolve(ctx: &Ctx<'_>, ty: &str) -> Result<String, String> {
    match ty.strip_prefix("@element:") {
        Some(element) => ctx
            .schema
            .elements
            .get(element)
            .cloned()
            .ok_or_else(|| format!("reference to undeclared element `{element}`")),
        None => Ok(ty.to_string()),
    }
}

fn rust_type(ctx: &Ctx<'_>, ty: &str, repeated: bool) -> Result<String, String> {
    let base = if let Some(prim) = builtin(ty) {
        prim.to_string()
    } else if ty == "ElementTagType" {
        "ElementTag".to_string()
    } else if ctx.consumed.contains(ty) {
        let union = ctx
            .schema
            .simple
            .iter()
            .find(|(_, d)| {
                matches!(d, SimpleDef::Union { members } if members.iter().any(|m| m == ty))
            })
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| ty.to_string());
        type_name(&union)
    } else {
        type_name(ty)
    };
    Ok(if repeated {
        format!("Vec<{base}>")
    } else {
        base
    })
}

fn builtin(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "xs:string"
        | "xs:normalizedString"
        | "xs:token"
        | "xs:language"
        | "xs:anyURI"
        | "xs:hexBinary"
        | "xs:base64Binary"
        | "xs:dateTime"
        | "xs:date"
        | "xs:time"
        | "xs:duration"
        | "xs:gYear"
        | "xs:NMTOKEN" => "String",
        "xs:boolean" => "bool",
        "xs:byte" => "i8",
        "xs:short" => "i16",
        "xs:int" => "i32",
        "xs:long" | "xs:integer" => "i64",
        "xs:unsignedByte" => "u8",
        "xs:unsignedShort" => "u16",
        "xs:unsignedInt" => "u32",
        "xs:unsignedLong" => "u64",
        "xs:float" => "f32",
        "xs:double" | "xs:decimal" => "f64",
        // SHIP's `data.payload` is `xs:anyType`: an opaque document belonging to the
        // protocol named by `data.header.protocolId`, which SHIP itself never inspects.
        "xs:anyType" => "serde_json::Value",
        _ => return None,
    })
}

fn dedupe_variants(values: &[String]) -> Vec<(String, String)> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        let mut variant = variant_name(v);
        let counter = seen.entry(variant.clone()).or_insert(0);
        *counter += 1;
        if *counter > 1 {
            variant = format!("{variant}{counter}");
        }
        out.push((v.clone(), variant));
    }
    out
}

fn write_modules(profile: &Profile, lowered: &Lowered) -> Result<(), String> {
    let out_dir = &profile.out_dir;
    if out_dir.exists() {
        std::fs::remove_dir_all(out_dir).map_err(|e| format!("clearing {out_dir:?}: {e}"))?;
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("creating {out_dir:?}: {e}"))?;

    for (module, src) in &lowered.modules {
        let path = out_dir.join(format!("{module}.rs"));
        std::fs::write(&path, src).map_err(|e| format!("writing {path:?}: {e}"))?;
    }

    let mut mod_rs = format!(
        "//! The generated {label} {version} data model.\n\
         //!\n\
         //! @generated by `cargo xtask codegen`. Do not edit by hand.\n\
         \n\
         #![allow(clippy::large_enum_variant)]\n\
         #![allow(clippy::enum_variant_names)]\n\
         #![allow(clippy::doc_markdown)]\n\
         \n",
        label = profile.label,
        version = profile.version,
    );
    for module in lowered.modules.keys() {
        writeln!(mod_rs, "mod {module};").unwrap();
    }
    mod_rs.push('\n');
    for module in lowered.modules.keys() {
        writeln!(mod_rs, "pub use {module}::*;").unwrap();
    }
    std::fs::write(out_dir.join("mod.rs"), mod_rs)
        .map_err(|e| format!("writing {out_dir:?}/mod.rs: {e}"))?;
    Ok(())
}

/// Emits the Restricted Function Exchange metadata: which elements identify an entry,
/// which function holds a list, how an elements filter maps onto its data type, and how
/// a selectors filter matches.
///
/// None of this is marked in the schemas. It is derived from the conventions SPINE
/// follows throughout — identifiers first, `XListData` holding `XData`, `XDataElements`
/// mirroring `XData` — so that the merge rules can be written once, generically, rather
/// than per use case.
fn emit_rfe_metadata(
    ctx: &Ctx<'_>,
    bodies: &mut BTreeMap<String, Vec<(String, String)>>,
    module_of: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    let schema = ctx.schema;

    for (list_name, list_def) in &schema.complex {
        let fields = flatten_fields(ctx, list_name, list_def, 0)?;
        let [only] = fields.as_slice() else { continue };
        let Some(item_xsd) = single_repeated_item(ctx, list_def)? else {
            continue;
        };
        if !schema.complex.contains_key(&item_xsd) || is_alias(ctx, list_name)? {
            continue;
        }

        let list_rust = type_name(list_name);
        let item_rust = type_name(&item_xsd);
        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_list!({list_rust} => {item_rust} {{ {} }});",
            only.rust_name
        )
        .unwrap();
        bodies
            .entry(module_of(list_name))
            .or_default()
            .push((format!("{list_name}~list"), text));

        // Identifiers of the entry type.
        let item_def = &schema.complex[&item_xsd];
        let identifiers = leading_identifiers(ctx, &item_xsd, item_def)?;
        if !identifiers.is_empty() && !is_alias(ctx, &item_xsd)? {
            let mut text = String::new();
            writeln!(
                text,
                "crate::eebus_identity!({item_rust} {{ {} }});",
                identifiers.join(", ")
            )
            .unwrap();
            bodies
                .entry(module_of(&item_xsd))
                .or_default()
                .push((format!("{item_xsd}~identity"), text));
        }
    }

    // Elements filters mirror their data type element for element. The top-level pairs
    // are found by name; nested ones are reached by following a `nested` field into the
    // types on both sides, because the schemas give those inline definitions synthetic
    // names that do not line up.
    let mut queue: Vec<(String, String)> = schema
        .complex
        .keys()
        .filter_map(|data_name| {
            let elements_name = format!(
                "{}ElementsType",
                data_name.strip_suffix("Type").unwrap_or(data_name)
            );
            schema
                .complex
                .contains_key(&elements_name)
                .then(|| (data_name.clone(), elements_name))
        })
        .collect();
    // One implementation per filter type. Where two data types share a filter — the
    // schemas do that for a handful — the pair found by name wins, so the queue is
    // worked front to back with the name-matched seeds first.
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut cursor = 0usize;

    while cursor < queue.len() {
        let (data_name, elements_name) = queue[cursor].clone();
        cursor += 1;
        // The implementation is attached to the filter type, so only that side must not
        // be an alias. A scalar data side is fine: the filter then names nothing and the
        // parent's emptiness check decides.
        if is_alias(ctx, &elements_name)? {
            continue;
        }
        if !emitted.insert(elements_name.clone()) {
            continue;
        }
        let (Some(data_def), Some(elements_def)) = (
            schema.complex.get(&data_name),
            schema.complex.get(&elements_name),
        ) else {
            continue;
        };
        let data_fields = flatten_fields(ctx, &data_name, data_def, 0)?;
        let element_fields = flatten_fields(ctx, &elements_name, elements_def, 0)?;

        let mut entries = Vec::new();
        for f in &element_fields {
            let Some(data_field) = data_fields.iter().find(|d| d.rust_name == f.rust_name) else {
                continue;
            };
            if f.rust_type == "ElementTag" {
                entries.push(format!("tag {}", f.rust_name));
            } else {
                // A filter for a repeating element addresses every occurrence of it.
                let kind = if data_field.rust_type.starts_with("Vec<") {
                    "nested_each"
                } else {
                    "nested"
                };
                entries.push(format!("{kind} {}", f.rust_name));
                let pair = (data_field.xsd_type.clone(), f.xsd_type.clone());
                if schema.complex.contains_key(&pair.0)
                    && schema.complex.contains_key(&pair.1)
                    && !emitted.contains(&pair.1)
                    && !queue.contains(&pair)
                {
                    queue.push(pair);
                }
            }
        }

        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_elements!({} => {} {{ {} }});",
            type_name(&data_name),
            type_name(&elements_name),
            entries.join(", ")
        )
        .unwrap();
        bodies
            .entry(module_of(&elements_name))
            .or_default()
            .push((format!("{elements_name}~elements"), text));
    }

    // Selectors filters match entries of the list they belong to.
    for (selectors_name, selectors_def) in &schema.complex {
        let Some(list_stem) = selectors_name.strip_suffix("SelectorsType") else {
            continue;
        };
        let list_name = format!("{list_stem}Type");
        let Some(list_def) = schema.complex.get(&list_name) else {
            continue;
        };
        let Some(item_xsd) = single_repeated_item(ctx, list_def)? else {
            continue;
        };
        let Some(item_def) = schema.complex.get(&item_xsd) else {
            continue;
        };
        if is_alias(ctx, selectors_name)? {
            continue;
        }

        let item_fields = flatten_fields(ctx, &item_xsd, item_def, 0)?;
        let selector_fields = flatten_fields(ctx, selectors_name, selectors_def, 0)?;

        let mut matched = Vec::new();
        let mut unsupported = Vec::new();
        for f in &selector_fields {
            match item_fields
                .iter()
                .find(|i| i.rust_name == f.rust_name && i.rust_type == f.rust_type)
            {
                Some(_) => matched.push(f.rust_name.clone()),
                None => unsupported.push(format!("{:?}", f.wire)),
            }
        }

        let mut text = String::new();
        writeln!(
            text,
            "crate::eebus_selectors!({} => {} {{ {} }} unsupported {{ {} }});",
            type_name(&item_xsd),
            type_name(selectors_name),
            matched.join(", "),
            unsupported.join(", ")
        )
        .unwrap();
        bodies
            .entry(module_of(selectors_name))
            .or_default()
            .push((format!("{selectors_name}~selectors"), text));
    }

    Ok(())
}

/// True for a complex type emitted as a type alias rather than a struct.
///
/// The schemas define a few types purely as a restriction of another, and one as an
/// `xs:simpleContent` wrapper. Those become `pub type` aliases, so attaching a trait
/// implementation to them would collide with the implementation on the type they name.
fn is_alias(ctx: &Ctx<'_>, name: &str) -> Result<bool, String> {
    let Some(def) = ctx.schema.complex.get(name) else {
        return Ok(false);
    };
    if def.simple_content.is_some() {
        return Ok(true);
    }
    Ok(def.restricts.is_some() && flatten_fields(ctx, name, def, 0)?.is_empty())
}

/// The item type of a complex type that holds exactly one repeated element.
fn single_repeated_item(ctx: &Ctx<'_>, def: &ComplexDef) -> Result<Option<String>, String> {
    if def.extends.is_some() || def.particles.len() != 1 {
        return Ok(None);
    }
    match &def.particles[0] {
        Particle::Element {
            ty, repeated: true, ..
        } => Ok(Some(resolve(ctx, ty)?)),
        _ => Ok(None),
    }
}

/// The leading run of elements whose type is a numeric identifier.
///
/// SPINE places an entry's primary and sub identifiers first, ahead of its payload; a
/// later identifier — `loadControlLimitDescriptionData.measurementId`, say — is a
/// *foreign* key into another feature and does not take part in this entry's identity.
fn leading_identifiers(
    ctx: &Ctx<'_>,
    owner: &str,
    def: &ComplexDef,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for particle in &def.particles {
        let Particle::Element {
            wire, ty, repeated, ..
        } = particle
        else {
            break;
        };
        if *repeated {
            break;
        }
        let resolved = resolve(ctx, ty)?;
        if !is_identifier_type(ctx, &resolved) {
            break;
        }
        out.push(field_name(wire));
    }
    let _ = owner;
    Ok(out)
}

/// True for a simple type named `*IdType` that restricts an integer.
fn is_identifier_type(ctx: &Ctx<'_>, ty: &str) -> bool {
    if !ty.ends_with("IdType") {
        return false;
    }
    matches!(
        ctx.schema.simple.get(ty),
        Some(SimpleDef::Restrict { base })
            if builtin(base).is_some_and(|p| p.starts_with('u') || p.starts_with('i'))
    )
}
