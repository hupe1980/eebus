//! Which schemas the committed model was generated from.
//!
//! The model is committed and the schemas are not, because they are not redistributable
//! (D42). So nothing can say whether the schemas on a maintainer's machine are the ones the
//! model came from, until something writes that down. `cargo xtask codegen` writes
//! `schemas.sha256` beside each generated model, and `cargo xtask check-schemas` compares
//! the schemas it finds against it.
//!
//! The record is `sha256sum` output: one `<digest>  <file name>` line per schema, sorted.
//! So `shasum -a 256 -c` can check it by hand without this crate.

use std::path::{Path, PathBuf};

/// The schema files the generator reads from a bundle, sorted: every `*.xsd` but the
/// `*_overview` ones, which only re-include the others.
///
/// The generator and the check both select through this one function, so the set of
/// files that is hashed cannot drift from the set that is read.
pub fn schema_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
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
    Ok(files)
}

/// The name of the record, in each generated directory.
pub const RECORD: &str = "schemas.sha256";

/// SHA-256 of a schema with its line endings normalised.
///
/// The generator parses XML, so a copy unpacked with CRLF line endings generates the same
/// model and is the same schema.
fn digest(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut rest = bytes.as_slice();
    while let Some(at) = rest.windows(2).position(|w| w == b"\r\n") {
        hasher.update(&rest[..at]);
        hasher.update(b"\n");
        rest = &rest[at + 2..];
    }
    hasher.update(rest);
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The record of a bundle: what [`RECORD`] holds.
pub fn record(dir: &Path) -> Result<String, String> {
    let mut out = String::new();
    for path in schema_files(dir)? {
        out.push_str(&format!("{}  {}\n", digest(&path)?, name(&path)));
    }
    Ok(out)
}

/// How a bundle on disk compares with a record.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every file matches; this many were compared.
    Match(usize),
    /// What differs, by file name.
    Differs {
        changed: Vec<String>,
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
}

/// Compares the bundle in `dir` with the text of a record.
pub fn compare(dir: &Path, recorded: &str) -> Result<Outcome, String> {
    use std::collections::BTreeMap;

    let parse = |text: &str| -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|line| line.split_once("  "))
            .map(|(digest, name)| (name.to_string(), digest.to_string()))
            .collect()
    };
    let expected = parse(recorded);
    let found = parse(&record(dir)?);

    let changed: Vec<_> = found
        .iter()
        .filter(|(name, digest)| expected.get(*name).is_some_and(|d| d != *digest))
        .map(|(name, _)| name.clone())
        .collect();
    let missing: Vec<_> = expected
        .keys()
        .filter(|name| !found.contains_key(*name))
        .cloned()
        .collect();
    let unexpected: Vec<_> = found
        .keys()
        .filter(|name| !expected.contains_key(*name))
        .cloned()
        .collect();

    if changed.is_empty() && missing.is_empty() && unexpected.is_empty() {
        Ok(Outcome::Match(found.len()))
    } else {
        Ok(Outcome::Differs {
            changed,
            missing,
            unexpected,
        })
    }
}

/// `cargo xtask check-schemas`: each bundle that is present against its committed record.
///
/// A bundle that is absent is reported and does not fail, which is what lets this run on
/// a machine without the specifications, CI included.
pub fn check(root: &Path) -> Result<String, String> {
    check_at(root, &crate::reference_dir(root))
}

/// [`check`], with the specifications read from `reference`.
fn check_at(root: &Path, reference: &Path) -> Result<String, String> {
    let mut report = String::new();
    let mut failures = Vec::new();
    for profile in crate::codegen::profiles_at(root, reference) {
        let label = profile.label;
        if !profile.xsd_dir.is_dir() {
            report.push_str(&format!(
                "{label}: not checked, no schemas at {}\n",
                profile.xsd_dir.display()
            ));
            continue;
        }
        let record_path = profile.out_dir.join(RECORD);
        let Ok(recorded) = std::fs::read_to_string(&record_path) else {
            failures.push(format!(
                "{label}: no committed {RECORD} beside the generated model; run cargo xtask codegen"
            ));
            continue;
        };
        match compare(&profile.xsd_dir, &recorded)? {
            Outcome::Match(n) => report.push_str(&format!(
                "{label}: {n} {} the committed record\n",
                if n == 1 {
                    "schema matches"
                } else {
                    "schemas match"
                }
            )),
            Outcome::Differs {
                changed,
                missing,
                unexpected,
            } => {
                let mut lines = vec![format!(
                    "{label}: the schemas at {} are not the ones the model was generated from",
                    profile.xsd_dir.display()
                )];
                for (kind, names) in [
                    ("changed", changed),
                    ("missing", missing),
                    ("unexpected", unexpected),
                ] {
                    lines.extend(names.into_iter().map(|n| format!("  {kind:<10} {n}")));
                }
                failures.push(lines.join("\n"));
            }
        }
    }
    if failures.is_empty() {
        Ok(report.trim_end().to_string())
    } else {
        // The failure first, so it is what `error:` introduces; the other bundles after.
        Err(format!("{}\n{}", failures.join("\n"), report.trim_end())
            .trim_end()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle of three schemas and an overview, in a directory of its own.
    struct Bundle(PathBuf);

    impl Bundle {
        fn new(test: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("eebus-schemas-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, body) in [
                ("B_Measurement.xsd", "<schema>measurement</schema>\n"),
                ("A_LoadControl.xsd", "<schema>load control</schema>\n"),
                ("C_Commondatatypes.xsd", "<schema>common</schema>\n"),
                ("EEBus_SPINE_TS_overview.xsd", "<schema>includes</schema>\n"),
                ("README.pdf", "not a schema"),
            ] {
                std::fs::write(dir.join(name), body).unwrap();
            }
            Self(dir)
        }

        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_record_lists_what_the_generator_reads() {
        let bundle = Bundle::new("record");
        let record = record(&bundle.0).unwrap();
        let names: Vec<_> = record
            .lines()
            .map(|l| l.split_once("  ").unwrap().1)
            .collect();
        assert_eq!(
            names,
            [
                "A_LoadControl.xsd",
                "B_Measurement.xsd",
                "C_Commondatatypes.xsd"
            ],
            "sorted, without the overview or anything that is not a schema"
        );
        for line in record.lines() {
            let (digest, _) = line.split_once("  ").unwrap();
            assert_eq!(digest.len(), 64, "sha256sum's format: {line}");
            assert!(
                digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
        assert!(record.ends_with('\n'));
    }

    #[test]
    fn a_matching_bundle_passes() {
        let bundle = Bundle::new("match");
        let recorded = record(&bundle.0).unwrap();
        assert_eq!(compare(&bundle.0, &recorded).unwrap(), Outcome::Match(3));
    }

    #[test]
    fn a_changed_byte_is_named() {
        let bundle = Bundle::new("changed");
        let recorded = record(&bundle.0).unwrap();
        bundle.write("B_Measurement.xsd", "<schema>measurementS</schema>\n");
        assert_eq!(
            compare(&bundle.0, &recorded).unwrap(),
            Outcome::Differs {
                changed: vec!["B_Measurement.xsd".into()],
                missing: vec![],
                unexpected: vec![],
            }
        );
    }

    #[test]
    fn a_missing_schema_is_named() {
        let bundle = Bundle::new("missing");
        let recorded = record(&bundle.0).unwrap();
        std::fs::remove_file(bundle.0.join("A_LoadControl.xsd")).unwrap();
        assert_eq!(
            compare(&bundle.0, &recorded).unwrap(),
            Outcome::Differs {
                changed: vec![],
                missing: vec!["A_LoadControl.xsd".into()],
                unexpected: vec![],
            }
        );
    }

    #[test]
    fn an_unexpected_schema_is_named() {
        let bundle = Bundle::new("unexpected");
        let recorded = record(&bundle.0).unwrap();
        bundle.write("D_Extra.xsd", "<schema/>\n");
        assert_eq!(
            compare(&bundle.0, &recorded).unwrap(),
            Outcome::Differs {
                changed: vec![],
                missing: vec![],
                unexpected: vec!["D_Extra.xsd".into()],
            }
        );
    }

    /// The generator's output does not depend on line endings, so neither does the digest.
    #[test]
    fn crlf_is_the_same_schema() {
        let bundle = Bundle::new("crlf");
        let recorded = record(&bundle.0).unwrap();
        bundle.write("A_LoadControl.xsd", "<schema>load control</schema>\r\n");
        assert_eq!(compare(&bundle.0, &recorded).unwrap(), Outcome::Match(3));
    }

    /// Absence is reported, not failed: what lets the check run without the
    /// specifications, CI included.
    #[test]
    fn an_absent_bundle_is_reported_and_passes() {
        let root =
            std::env::temp_dir().join(format!("eebus-schemas-absent-{}", std::process::id()));
        let report = check_at(&root, &root.join("nowhere")).expect("absence is not a failure");
        assert!(report.contains("SPINE: not checked"), "{report}");
        assert!(report.contains("SHIP: not checked"), "{report}");
    }
}
