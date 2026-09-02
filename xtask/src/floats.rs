//! Keeping float arithmetic out of the layers that convert the wire.
//!
//! Two defects reached the wire conversion, and both were the same shape: arithmetic on a
//! quantity, written by hand, in a place where being off by an ulp is being off by a
//! limit. `to_f64` multiplied by `10^scale` and rounded twice; `from_f64` cast with `as`
//! and saturated to `i64::MAX`. Every test passed, because the tests asserted behaviour
//! and the arithmetic was the thing that was wrong.
//!
//! A blanket ban on `f64` is the wrong tool for this crate — the use-case API is watts and
//! amperes as `f64` on purpose, and always will be. What is bannable is the narrow surface
//! both defects were on: **hand-rolled powers of ten and float-to-integer casts inside
//! `src/model/` and `src/codec/`**, where the conversion between the wire and a number
//! lives.
//!
//! `src/model/values.rs` is the one exception, because it *is* the audited conversion.
//! Everything else in those two directories should be reaching for it rather than
//! reimplementing it.

use std::fmt::Write as _;
use std::path::Path;

/// Directories where the wire meets arithmetic.
const GUARDED: &[&str] = &["src/model", "src/codec"];

/// The audited conversion itself, and the generated model, which contains no arithmetic.
const EXEMPT: &[&str] = &["src/model/values.rs", "src/model/generated"];

/// What a hand-rolled conversion looks like.
const BANNED: &[(&str, &str)] = &[
    (
        "as f64",
        "cast to `f64`; use `ScaledNumber::to_f64`, which rounds once",
    ),
    (
        "as i64",
        "cast from a float saturates — `1e30 as i64` is `i64::MAX`",
    ),
    ("as i32", "cast from a float saturates"),
    ("as u64", "cast from a float saturates"),
    (
        "powi(",
        "a hand-rolled power of ten; `values::pow10` reads exact ones from a table",
    ),
    ("powf(", "a hand-rolled power of ten"),
    (
        "10.0",
        "a literal ten to multiply or divide by; that is what `pow10` is for",
    ),
];

pub fn run(root: &Path) -> Result<String, String> {
    let mut findings = Vec::new();
    let mut scanned = 0usize;

    for dir in GUARDED {
        for path in rust_files(&root.join(dir))? {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if EXEMPT.iter().any(|e| relative.starts_with(e)) {
                continue;
            }
            scanned += 1;
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            for (number, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                for (needle, why) in BANNED {
                    if code.contains(needle) {
                        findings.push(format!("{relative}:{}: `{needle}` — {why}", number + 1));
                    }
                }
            }
        }
    }

    if !findings.is_empty() {
        let mut report = String::from("float arithmetic where the wire is converted:\n");
        for finding in &findings {
            let _ = writeln!(report, "  {finding}");
        }
        report.push_str(
            "\nIf this is the audited conversion moving house, add its file to EXEMPT in \
             xtask/src/floats.rs and say why in the commit.\n",
        );
        return Err(report);
    }

    Ok(format!(
        "no hand-rolled float arithmetic in {scanned} files under {}",
        GUARDED.join(", ")
    ))
}

fn rust_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {dir:?}: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
