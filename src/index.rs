//! Sparse registry index types and parsing utilities.
//!
//! The crates.io sparse registry index is available at `https://index.crates.io/`.
//! Each crate has a file at a deterministic path containing one JSON object per
//! line, where each line describes a single published version of that crate.
//!
//! # Path format
//!
//! | name length | path                               |
//! |-------------|-----------------------------------|
//! | 1           | `1/{name}`                         |
//! | 2           | `2/{name}`                         |
//! | 3           | `3/{first_char}/{name}`            |
//! | 4+          | `{chars[0..2]}/{chars[2..4]}/{name}` |
//!
//! All names are lower-cased before computing the path.

use serde_derive::Deserialize;
use std::collections::HashMap;

/// A single version entry from the sparse registry index.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct IndexEntry {
    /// Crate name (canonical casing from the registry).
    pub name: String,
    /// Version string (semver).
    pub vers: String,
    /// Dependencies declared by this version.
    #[serde(default)]
    pub deps: Vec<IndexDep>,
    /// SHA-256 checksum of the `.crate` archive.
    pub cksum: String,
    /// Feature definitions (v1 format).
    #[serde(default)]
    pub features: HashMap<String, Vec<String>>,
    /// Whether this version has been yanked.
    #[serde(default)]
    pub yanked: bool,
    /// Optional `links` key value from `Cargo.toml`.
    pub links: Option<String>,
    /// Index format version (1 = legacy, 2 = supports `features2`).
    #[serde(default)]
    pub v: u32,
    /// Additional features using namespaced syntax (v2+, e.g. `dep:crate`).
    #[serde(default)]
    pub features2: HashMap<String, Vec<String>>,
}

impl IndexEntry {
    /// Return the merged feature map combining `features` and `features2`.
    ///
    /// In index format v2, some feature values use the `dep:` namespace and
    /// are stored in `features2` to preserve backwards compatibility.  Both
    /// maps represent the same logical feature table and should be merged
    /// before being surfaced to callers.
    pub fn merged_features(&self) -> HashMap<String, Vec<String>> {
        if self.features2.is_empty() {
            return self.features.clone();
        }
        let mut merged = self.features.clone();
        for (k, v) in &self.features2 {
            merged
                .entry(k.clone())
                .or_insert_with(Vec::new)
                .extend(v.iter().cloned());
        }
        merged
    }
}

/// A dependency entry inside an [`IndexEntry`].
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct IndexDep {
    /// The name used in `Cargo.toml` (may differ from the actual package name
    /// when the dep is renamed via the `package` field).
    pub name: String,
    /// Semver version requirement string.
    pub req: String,
    /// Cargo features explicitly enabled for this dependency.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether default features of the dependency are enabled.
    #[serde(default = "default_true")]
    pub default_features: bool,
    /// Whether this is an optional dependency.
    #[serde(default)]
    pub optional: bool,
    /// Target platform filter (`cfg(...)` expression or target triple), if any.
    pub target: Option<String>,
    /// Dependency kind: `"normal"`, `"dev"`, or `"build"`.
    pub kind: Option<String>,
    /// Registry URL when the dependency comes from a non-default registry.
    pub registry: Option<String>,
    /// Actual package name when the dependency has been renamed.
    pub package: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Parse the newline-delimited JSON content of a sparse index file.
///
/// Each non-empty line is expected to be a JSON object representing one
/// version of the crate.  Lines that fail to parse are returned as errors.
///
/// # Errors
///
/// Returns the first [`serde_json::Error`] encountered.
pub fn parse_index_file(content: &str) -> Result<Vec<IndexEntry>, serde_json::Error> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Compute the path segment for a crate in the sparse index.
///
/// The path is relative to the index base URL (`https://index.crates.io/`).
///
/// # Panics
///
/// Does not panic; returns an empty string for an empty name.
pub fn index_path(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.len() {
        0 => String::new(),
        1 => format!("1/{}", lower),
        2 => format!("2/{}", lower),
        3 => format!("3/{}/{}", &lower[..1], lower),
        _ => format!("{}/{}/{}", &lower[..2], &lower[2..4], lower),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_path_one_char() {
        assert_eq!(index_path("a"), "1/a");
    }

    #[test]
    fn test_index_path_two_chars() {
        assert_eq!(index_path("cc"), "2/cc");
    }

    #[test]
    fn test_index_path_three_chars() {
        assert_eq!(index_path("log"), "3/l/log");
    }

    #[test]
    fn test_index_path_four_chars() {
        assert_eq!(index_path("toml"), "to/ml/toml");
    }

    #[test]
    fn test_index_path_long() {
        assert_eq!(index_path("serde"), "se/rd/serde");
        assert_eq!(index_path("reqwest"), "re/qw/reqwest");
    }

    #[test]
    fn test_index_path_uppercase_normalised() {
        assert_eq!(index_path("Serde"), "se/rd/serde");
    }

    #[test]
    fn test_parse_index_file() {
        let content = r#"{"name":"foo","vers":"0.1.0","deps":[],"cksum":"abc","features":{},"yanked":false}
{"name":"foo","vers":"0.2.0","deps":[],"cksum":"def","features":{},"yanked":false}
"#;
        let entries = parse_index_file(content).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].vers, "0.1.0");
        assert_eq!(entries[1].vers, "0.2.0");
    }

    #[test]
    fn test_merged_features_v2() {
        let entry: IndexEntry = serde_json::from_str(
            r#"{
                "name":"foo","vers":"1.0.0","deps":[],"cksum":"abc",
                "features":{"default":["std"],"std":[]},
                "yanked":false,
                "v":2,
                "features2":{"derive":["dep:foo_derive"]}
            }"#,
        )
        .unwrap();
        let merged = entry.merged_features();
        assert!(merged.contains_key("default"));
        assert!(merged.contains_key("std"));
        assert!(merged.contains_key("derive"));
        assert_eq!(merged["derive"], vec!["dep:foo_derive"]);
    }
}
