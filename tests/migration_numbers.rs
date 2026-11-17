//! Two lanes writing this repo gave two migration files the same number (both `179_`), and the app refused to
//! serve: sqlx saw version 179 already applied with different content and crash-looped in production.
//!
//! A duplicate migration number is an OUTAGE, not a style issue, so it fails here in the test run instead of at
//! boot. This lives as an integration test on purpose: it reads the migrations directory on disk, so it cannot be
//! defeated by a module being moved or a helper becoming unused.

use std::collections::BTreeMap;

#[test]
fn no_two_migrations_share_a_number() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut seen: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("migrations directory") {
        let name = entry
            .expect("dir entry")
            .file_name()
            .to_string_lossy()
            .to_string();
        if !name.ends_with(".sql") {
            continue;
        }
        let number: u32 = name
            .split('_')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("migration without a numeric prefix: {name}"));
        seen.entry(number).or_default().push(name);
    }
    let clashes: Vec<String> = seen
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(n, files)| format!("{n}: {}", files.join(", ")))
        .collect();
    assert!(
        clashes.is_empty(),
        "two migrations share a version number — sqlx will refuse to boot: {}",
        clashes.join(" | ")
    );
    assert!(
        seen.len() > 100,
        "the migrations directory was not read: {} files",
        seen.len()
    );
}
