//! Turning captures from real devices into wire-format fixtures.
//!
//! Every other fixture in this repository was produced by this crate or by the
//! specification. These come from somebody else's implementation talking to somebody
//! else's hardware: [`enbility/devices`](https://github.com/enbility/devices) records the
//! answers real EEBUS devices give to the only two questions a SHIP connection opens
//! with — *what are you?* and *what do you do?* — captured with `eebus-go`.
//!
//! They are recorded as ordinary JSON, the way a Go struct serialises, rather than in the
//! EEBUS JSON-UTF8 projection SHIP §11.4 defines. This converts between the two, which is
//! a mechanical transformation:
//!
//! * an object becomes an **array of single-key objects**, in key order;
//! * a repeated *complex* element becomes an **array of those arrays**;
//! * a repeated *scalar* element stays a plain array — `{"entity":[0]}`, not
//!   `{"entity":[{...}]}`;
//! * an empty object is an element with no content, which is `[]`.
//!
//! Usage: clone the corpus and point at it.
//!
//! ```sh
//! git clone https://depth=1 https://github.com/enbility/devices /tmp/devices
//! cargo run -p xtask -- devices /tmp/devices
//! ```
//!
//! The corpus is MIT-licensed; `tests/fixtures/devices/README.md` carries the attribution.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub fn run(root: &Path, corpus: &Path) -> Result<String, String> {
    if !corpus.is_dir() {
        return Err(format!(
            "no corpus at {}; clone https://github.com/enbility/devices first",
            corpus.display()
        ));
    }
    let out_dir = root.join("tests/fixtures/devices");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("creating {out_dir:?}: {e}"))?;

    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for (source, name) in captures(corpus)? {
        let text = std::fs::read_to_string(&source)
            .map_err(|e| format!("reading {}: {e}", source.display()))?;
        // A capture that is not valid JSON is upstream's to fix, not ours to guess at.
        let Ok(raw) = serde_json::from_str::<Value>(&text) else {
            skipped.push(name);
            continue;
        };
        let Some(datagram) = raw.pointer("/data/payload/datagram") else {
            skipped.push(name);
            continue;
        };
        let wire = Value::Array(vec![single("datagram", to_wire(datagram))]);
        let mut json = serde_json::to_string(&wire).map_err(|e| e.to_string())?;
        // The outermost value is the `datagram` element itself, not a sequence of one.
        json = json
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(&json)
            .to_string();
        json.push('\n');
        std::fs::write(out_dir.join(&name), json).map_err(|e| format!("writing {name}: {e}"))?;
        written.push(name);
    }

    if written.is_empty() {
        return Err(format!("no captures found under {}", corpus.display()));
    }

    let mut report = String::new();
    let _ = writeln!(
        report,
        "{} fixtures written to {}",
        written.len(),
        out_dir.display()
    );
    for name in &written {
        let _ = writeln!(report, "  {name}");
    }
    for name in &skipped {
        let _ = writeln!(report, "  skipped (not a datagram): {name}");
    }
    Ok(report)
}

/// The `discovery-data.json` and `usecase-data.json` under every `<brand>/<device>/`.
fn captures(corpus: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut found = Vec::new();
    let brands = std::fs::read_dir(corpus).map_err(|e| format!("reading {corpus:?}: {e}"))?;
    for brand in brands.flatten() {
        if !brand.path().is_dir() {
            continue;
        }
        let brand_name = brand.file_name().to_string_lossy().into_owned();
        if brand_name.starts_with('.') || brand_name == "schema" {
            continue;
        }
        let devices = std::fs::read_dir(brand.path())
            .map_err(|e| format!("reading {:?}: {e}", brand.path()))?;
        for device in devices.flatten() {
            if !device.path().is_dir() {
                continue;
            }
            let device_name = device.file_name().to_string_lossy().into_owned();
            for (file, suffix) in [
                ("discovery-data.json", "discovery"),
                ("usecase-data.json", "usecase"),
            ] {
                let path = device.path().join(file);
                if path.is_file() {
                    let stem = sanitise(&format!("{brand_name}_{device_name}"));
                    found.push((path, format!("{stem}.{suffix}.json")));
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// A file name that is the same on every platform a contributor might use.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == ':' || c == '/' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// The EEBUS JSON-UTF8 projection of SHIP §11.4.
fn to_wire(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Array(
            fields
                .iter()
                .map(|(key, val)| single(key, to_wire(val)))
                .collect(),
        ),
        // A repeated complex element is an array of sequences; a repeated scalar one
        // stays a plain array, which is why `{"entity":[0]}` is not wrapped.
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            Value::Array(items.iter().map(to_wire).collect())
        }
        other => other.clone(),
    }
}

fn single(key: &str, value: Value) -> Value {
    let mut object = Map::new();
    object.insert(key.to_string(), value);
    Value::Object(object)
}
