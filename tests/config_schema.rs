//! CI validation: every field read by `Config` is both *registered*
//! (parses cleanly from `prodder.example.toml`) and *documented*
//! (has a preceding TOML comment line).
//!
//! This test is deliberately pure Rust — no extra tooling — so the
//! husky pre-commit hook can run it alongside the existing
//! `cargo test` invocation.

use std::collections::BTreeSet;

use prodder::config::Config;

const EXAMPLE_PATH: &str = "prodder.example.toml";

fn read_example() -> String {
    std::fs::read_to_string(EXAMPLE_PATH).unwrap_or_else(|e| {
        panic!(
            "failed to read {EXAMPLE_PATH}: {e}. The schema test \
             must be run from the repository root."
        )
    })
}

#[test]
fn example_parses_as_default_config() {
    let raw = read_example();
    let cfg: Config = toml::from_str(&raw)
        .expect("example should parse into Config");
    assert_eq!(
        cfg,
        Config::default(),
        "prodder.example.toml should serialize to the same values \
         as Config::default() — keep the example in sync with the \
         documented defaults"
    );
}

/// Reflect on `Config` by round-tripping through `toml::Value` so
/// the test does not need to list field names by hand — whatever
/// serde emits is what we assert the example documents.
fn config_field_paths() -> BTreeSet<String> {
    let cfg = Config::default();
    let v = toml::Value::try_from(&cfg)
        .expect("Config serializes to toml::Value");
    let mut out = BTreeSet::new();
    collect_paths("", &v, &mut out);
    out
}

fn collect_paths(
    prefix: &str,
    v: &toml::Value,
    out: &mut BTreeSet<String>,
) {
    if let toml::Value::Table(t) = v {
        for (k, child) in t.iter() {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            out.insert(path.clone());
            collect_paths(&path, child, out);
        }
    }
}

/// The leaf key of a dotted path (`a.b.c` → `c`).
fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

#[test]
fn every_config_field_appears_in_example() {
    let raw = read_example();
    let paths = config_field_paths();
    for path in &paths {
        let leaf_key = leaf(path);
        // Match either a table header `[foo]` or a key assignment
        // `foo = ...` at the start of a (possibly indented) line.
        let key_pattern =
            format!("\n{leaf_key} =").replace('\n', "\n");
        let header_pattern = format!("[{leaf_key}]");
        let found = raw.contains(&key_pattern)
            || raw.starts_with(&format!("{leaf_key} ="))
            || raw.contains(&header_pattern);
        assert!(
            found,
            "field `{path}` is not present in \
             prodder.example.toml — update the example to keep it \
             in sync with Config"
        );
    }
}

#[test]
fn every_config_field_has_a_preceding_comment() {
    let raw = read_example();
    let lines: Vec<&str> = raw.lines().collect();
    let paths = config_field_paths();

    for path in &paths {
        let leaf_key = leaf(path);
        // Find the line declaring this field (either `key =` or
        // `[key]`).
        let idx = lines.iter().position(|l| {
            let trimmed = l.trim_start();
            trimmed.starts_with(&format!("{leaf_key} ="))
                || trimmed == format!("[{leaf_key}]")
        });
        let idx = idx.unwrap_or_else(|| {
            panic!(
                "field `{path}` not found in \
                 prodder.example.toml"
            )
        });

        // Walk backwards over blank lines; the first non-blank
        // preceding line must be a TOML comment.
        let mut cursor = idx;
        let mut saw_comment = false;
        while cursor > 0 {
            cursor -= 1;
            let trimmed = lines[cursor].trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                saw_comment = true;
            }
            break;
        }
        assert!(
            saw_comment,
            "field `{path}` at line {} of \
             prodder.example.toml must be preceded by a `#` \
             documentation comment",
            idx + 1
        );
    }
}
