//! Every public future says what cancelling it does.
//!
//! A future in a `select!` or under a `timeout` is dropped at whichever `await` it was
//! parked on. Whether that loses anything is a property of the function, invisible in its
//! signature — so it is written in its documentation, and this holds the line: the doc
//! comment of every `pub async fn` under `src/` contains "cancel" ("Cancel-safe", or what
//! cancelling it does). A lint, not a proof; the tests are the proof.

use std::path::Path;

pub(crate) fn run(root: &Path) -> Result<String, String> {
    let mut silent = Vec::new();
    let mut checked = 0;
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                for (line, name) in undocumented(&text, &mut checked) {
                    silent.push(format!("{relative}:{line}: pub async fn {name}"));
                }
            }
        }
    }
    if silent.is_empty() {
        return Ok(format!(
            "✓ {checked} public async fns, each saying what cancelling it does"
        ));
    }
    silent.sort();
    Err(format!(
        "these public async fns do not say whether they are cancel-safe:\n  {}",
        silent.join("\n  ")
    ))
}

/// `(line, name)` of each `pub async fn` in `text` whose doc comment never mentions
/// cancellation; `checked` counts every one seen.
fn undocumented(text: &str, checked: &mut usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("pub async fn ") else {
            continue;
        };
        *checked += 1;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let documented = lines[..index]
            .iter()
            .rev()
            .map(|l| l.trim_start())
            .take_while(|l| l.starts_with("///") || l.starts_with("#["))
            .any(|l| l.to_lowercase().contains("cancel"));
        if !documented {
            found.push((index + 1, name));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::undocumented;

    #[test]
    fn a_future_that_does_not_say_is_reported() {
        let source = "/// Reads.\n///\n/// Cancel-safe.\npub async fn good() {}\n\n/// Writes.\n#[must_use]\npub async fn silent() {}\n";
        let mut checked = 0;
        assert_eq!(
            undocumented(source, &mut checked),
            vec![(8, "silent".to_string())]
        );
        assert_eq!(checked, 2);
    }
}
