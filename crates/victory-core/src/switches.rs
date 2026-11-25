//! The `VICTORY_*` environment switches the code reads.
//!
//! The list is `docs/env.md`, which `cargo xtask env` generates from the
//! source (and a test keeps current). A set variable with the prefix that
//! is not in it is read by nothing: usually a typo, or a switch that was
//! renamed or removed, and silently without effect.

use std::ffi::OsString;

const TABLE: &str = include_str!("../../../docs/env.md");
const PREFIX: &str = "VICTORY_";

/// Every switch in the table.
pub fn known() -> impl Iterator<Item = &'static str> {
    TABLE
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .filter(|n| n.starts_with(PREFIX))
}

/// The names in `vars` with the prefix that no code reads, sorted.
pub fn unread(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<String> {
    let known: std::collections::HashSet<_> = known().collect();
    let mut unread: Vec<String> = vars
        .into_iter()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with(PREFIX) && !known.contains(k.as_str()))
        .collect();
    unread.sort();
    unread
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_lists_the_log_filter() {
        // The log filter's own switch, read in logging.rs.
        let log = format!("{PREFIX}LOG");
        assert!(known().any(|n| n == log));
        assert!(known().count() > 100);
    }

    #[test]
    fn unknown_names_with_the_prefix_are_reported() {
        let var = |k: &str| (OsString::from(k), OsString::from("1"));
        let typo = format!("{PREFIX}LOGG");
        let vars = [
            var(&format!("{PREFIX}LOG")),
            var(&typo),
            var("PATH"),
            var("RUST_LOG"),
        ];
        assert_eq!(unread(vars), [typo]);
    }
}
