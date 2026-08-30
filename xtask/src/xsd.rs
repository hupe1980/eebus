//! A small XML Schema reader, scoped to the subset SPINE uses.
//!
//! SPINE's schemas are machine-generated and stay within a narrow slice of XSD:
//! `xs:simpleType` with `xs:restriction`/`xs:union`, `xs:complexType` with
//! `xs:sequence`/`xs:choice`, `xs:complexContent` extension and restriction, global
//! `xs:element` declarations, and `xs:group` references. Handling exactly that keeps
//! this reader auditable, where a general XSD toolchain would not be.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum SimpleDef {
    /// `xs:restriction base="xs:string"` with `xs:enumeration` children.
    Enum { values: Vec<String> },
    /// `xs:restriction` of another type without enumerations.
    Restrict { base: String },
    /// `xs:union memberTypes="..."`.
    Union { members: Vec<String> },
    /// `xs:list itemType="..."`.
    List { item: String },
}

#[derive(Debug, Clone)]
pub enum Particle {
    Element {
        /// The element's name on the wire.
        wire: String,
        /// The element's type, already resolved through `ref=` where needed.
        ty: String,
        repeated: bool,
    },
    /// `xs:group ref="..."`, expanded during lowering.
    Group { name: String, repeated: bool },
}

#[derive(Debug, Clone, Default)]
pub struct ComplexDef {
    /// Base type of an `xs:extension`; its particles precede this type's own.
    pub extends: Option<String>,
    /// Base type of a `xs:restriction` in `xs:complexContent`, which only narrows.
    pub restricts: Option<String>,
    /// Base type of an `xs:simpleContent` extension: the type *is* that scalar, with
    /// attributes that the JSON mapping discards (SHIP §11.4.6 rule 7).
    pub simple_content: Option<String>,
    pub particles: Vec<Particle>,
}

#[derive(Debug, Clone)]
pub struct GroupDef {
    pub is_choice: bool,
    pub particles: Vec<Particle>,
}

#[derive(Debug, Default)]
pub struct Schema {
    pub simple: BTreeMap<String, SimpleDef>,
    pub complex: BTreeMap<String, ComplexDef>,
    pub groups: BTreeMap<String, GroupDef>,
    /// Global `xs:element` name → type name.
    pub elements: BTreeMap<String, String>,
    /// Type name → module the type is emitted into.
    pub module_of: BTreeMap<String, String>,
    /// Module name → the schema file it came from, in load order.
    pub modules: Vec<String>,
}

/// Strips the `ns_p:` / `xs:` prefix from a QName, keeping the `xs:` marker for builtins.
pub fn local(qname: &str) -> &str {
    match qname.split_once(':') {
        Some(("xs", _)) => qname,
        Some((_, rest)) => rest,
        None => qname,
    }
}

pub fn load_dir(dir: &Path, module_of: impl Fn(&str) -> String) -> Result<Schema, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "xsd"))
        .filter(|p| {
            !p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("_overview"))
        })
        .collect();
    files.sort();

    let mut schema = Schema::default();
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad file name: {}", path.display()))?;
        let module = module_of(stem);
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc = roxmltree::Document::parse(&text)
            .map_err(|e| format!("{}: parsing XML: {e}", path.display()))?;
        parse_schema(&doc, &module, &mut schema)?;
        if !schema.modules.contains(&module) {
            schema.modules.push(module);
        }
    }
    Ok(schema)
}

fn parse_schema(
    doc: &roxmltree::Document<'_>,
    module: &str,
    out: &mut Schema,
) -> Result<(), String> {
    let root = doc.root_element();
    for node in root.children().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "simpleType" => {
                let name = required_name(&node)?;
                let def = parse_simple(&node)?;
                out.simple.insert(name.clone(), def);
                out.module_of.insert(name, module.to_string());
            }
            "complexType" => {
                let name = required_name(&node)?;
                let mut anon = Vec::new();
                let def = parse_complex(&node, &name, &mut anon)?;
                for (anon_name, anon_def) in anon {
                    out.complex.insert(anon_name.clone(), anon_def);
                    out.module_of.insert(anon_name, module.to_string());
                }
                out.complex.insert(name.clone(), def);
                out.module_of.insert(name, module.to_string());
            }
            "element" => {
                let name = required_name(&node)?;
                if let Some(ty) = node.attribute("type") {
                    out.elements.insert(name, local(ty).to_string());
                } else if let Some(inline) = child(&node, "complexType") {
                    let synth = format!("{}Type", crate::naming::pascal(&name));
                    let mut anon = Vec::new();
                    let def = parse_complex(&inline, &synth, &mut anon)?;
                    for (anon_name, anon_def) in anon {
                        out.complex.insert(anon_name.clone(), anon_def);
                        out.module_of.insert(anon_name, module.to_string());
                    }
                    out.complex.insert(synth.clone(), def);
                    out.module_of.insert(synth.clone(), module.to_string());
                    out.elements.insert(name, synth);
                }
            }
            "group" => {
                let name = required_name(&node)?;
                let body = node
                    .children()
                    .find(|n| {
                        n.is_element()
                            && matches!(n.tag_name().name(), "sequence" | "choice" | "all")
                    })
                    .ok_or_else(|| format!("group {name} has no compositor"))?;
                let is_choice = body.tag_name().name() == "choice";
                let mut anon = Vec::new();
                let particles = parse_particles(&body, &name, &mut anon)?;
                for (anon_name, anon_def) in anon {
                    out.complex.insert(anon_name.clone(), anon_def);
                    out.module_of.insert(anon_name, module.to_string());
                }
                out.groups.insert(
                    name,
                    GroupDef {
                        is_choice,
                        particles,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn required_name(node: &roxmltree::Node<'_, '_>) -> Result<String, String> {
    node.attribute("name")
        .map(str::to_string)
        .ok_or_else(|| format!("<{}> without a name", node.tag_name().name()))
}

fn child<'a, 'input>(
    node: &roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
}

fn parse_simple(node: &roxmltree::Node<'_, '_>) -> Result<SimpleDef, String> {
    if let Some(union) = child(node, "union") {
        let members = union
            .attribute("memberTypes")
            .unwrap_or_default()
            .split_whitespace()
            .map(|m| local(m).to_string())
            .collect();
        return Ok(SimpleDef::Union { members });
    }
    if let Some(list) = child(node, "list") {
        let item = list
            .attribute("itemType")
            .map(|t| local(t).to_string())
            .unwrap_or_else(|| "xs:string".to_string());
        return Ok(SimpleDef::List { item });
    }
    if let Some(restriction) = child(node, "restriction") {
        let values: Vec<String> = restriction
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "enumeration")
            .filter_map(|n| n.attribute("value").map(str::to_string))
            .collect();
        if !values.is_empty() {
            return Ok(SimpleDef::Enum { values });
        }
        let base = restriction
            .attribute("base")
            .map(|b| local(b).to_string())
            .unwrap_or_else(|| "xs:string".to_string());
        return Ok(SimpleDef::Restrict { base });
    }
    Err("simpleType without union, list or restriction".to_string())
}

fn parse_complex(
    node: &roxmltree::Node<'_, '_>,
    owner: &str,
    anon: &mut Vec<(String, ComplexDef)>,
) -> Result<ComplexDef, String> {
    let mut def = ComplexDef::default();

    if let Some(sc) = child(node, "simpleContent") {
        if let Some(ext) = child(&sc, "extension") {
            def.simple_content = ext.attribute("base").map(|b| local(b).to_string());
            return Ok(def);
        }
        if let Some(res) = child(&sc, "restriction") {
            def.simple_content = res.attribute("base").map(|b| local(b).to_string());
            return Ok(def);
        }
    }

    if let Some(cc) = child(node, "complexContent") {
        if let Some(ext) = child(&cc, "extension") {
            def.extends = ext.attribute("base").map(|b| local(b).to_string());
            for body in ext
                .children()
                .filter(|n| n.is_element() && matches!(n.tag_name().name(), "sequence" | "choice"))
            {
                def.particles.extend(parse_particles(&body, owner, anon)?);
            }
            return Ok(def);
        }
        if let Some(res) = child(&cc, "restriction") {
            def.restricts = res.attribute("base").map(|b| local(b).to_string());
            for body in res
                .children()
                .filter(|n| n.is_element() && matches!(n.tag_name().name(), "sequence" | "choice"))
            {
                def.particles.extend(parse_particles(&body, owner, anon)?);
            }
            return Ok(def);
        }
    }

    for body in node
        .children()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "sequence" | "choice" | "all"))
    {
        def.particles.extend(parse_particles(&body, owner, anon)?);
    }
    Ok(def)
}

fn parse_particles(
    body: &roxmltree::Node<'_, '_>,
    owner: &str,
    anon: &mut Vec<(String, ComplexDef)>,
) -> Result<Vec<Particle>, String> {
    let mut out = Vec::new();
    for node in body.children().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "element" => {
                let repeated = is_repeated(&node);
                if let Some(r) = node.attribute("ref") {
                    let wire = local(r).to_string();
                    out.push(Particle::Element {
                        ty: format!("@element:{wire}"),
                        wire,
                        repeated,
                    });
                } else {
                    let wire = required_name(&node)?;
                    let ty = if let Some(t) = node.attribute("type") {
                        local(t).to_string()
                    } else if let Some(inline) = child(&node, "complexType") {
                        let owner_base = owner.strip_suffix("Type").unwrap_or(owner);
                        let synth = format!("{}{}Type", owner_base, crate::naming::pascal(&wire));
                        let def = parse_complex(&inline, &synth, anon)?;
                        anon.push((synth.clone(), def));
                        synth
                    } else {
                        // An element with neither type nor inline definition carries no
                        // content in SPINE; treat it as a presence tag.
                        "ElementTagType".to_string()
                    };
                    out.push(Particle::Element { wire, ty, repeated });
                }
            }
            "group" => {
                if let Some(r) = node.attribute("ref") {
                    out.push(Particle::Group {
                        name: local(r).to_string(),
                        repeated: is_repeated(&node),
                    });
                }
            }
            "sequence" | "choice" | "all" => {
                out.extend(parse_particles(&node, owner, anon)?);
            }
            _ => {}
        }
    }
    Ok(out)
}

fn is_repeated(node: &roxmltree::Node<'_, '_>) -> bool {
    match node.attribute("maxOccurs") {
        Some("unbounded") => true,
        Some(n) => n.parse::<u32>().map(|v| v > 1).unwrap_or(false),
        None => false,
    }
}
