//! Conversion helpers: sparse index types → public crates_io_api types.
//!
//! # Conversion process
//!
//! The sparse registry index stores the minimal data Cargo needs to resolve
//! dependency graphs.  The richer metadata exposed by the crates.io web API
//! (download counts, owners, categories, keywords, …) is **not** present in
//! the index.  This module converts what *is* available and fills the
//! remaining fields with sensible zero/empty defaults so that the resulting
//! types satisfy the same public interface.
//!
//! | Index field           | Target type field         | Notes                          |
//! |-----------------------|---------------------------|--------------------------------|
//! | `name`                | `Crate::name`, `::id`     | Canonical casing from index    |
//! | `vers`                | `Version::num`            | Semver string                  |
//! | `deps`                | `Vec<Dependency>`         | Full dep list per version      |
//! | `features`/`features2`| `Version::features`       | Merged; v2 namespaced included |
//! | `yanked`              | `Version::yanked`         | Boolean flag                   |
//! | `cksum`               | —                         | Not surfaced in public types   |
//! | (absent)              | `downloads`, `license`, … | Defaulted to 0 / None          |

use chrono::{DateTime, Utc};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::index::{IndexDep, IndexEntry};
use crate::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a stable synthetic `u64` ID from a crate name and version string.
///
/// The sparse index does not assign numeric IDs to versions; we derive one
/// deterministically so that the public `Version::id` field remains a `u64`.
pub fn synthesize_id(name: &str, version: &str) -> u64 {
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    // Separator ensures "ab" + "cd" ≠ "a" + "bcd".
    '\x00'.hash(&mut h);
    version.hash(&mut h);
    h.finish()
}

/// Return the Unix epoch (1970-01-01T00:00:00Z) as a UTC `DateTime`.
///
/// Used as a placeholder for timestamp fields that are not available in the
/// sparse index.
pub fn epoch() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

// ---------------------------------------------------------------------------
// Version helpers
// ---------------------------------------------------------------------------

/// Find the highest non-yanked version string from a slice of index entries.
///
/// Falls back to considering yanked versions if every entry is yanked.
/// Returns an empty string when `entries` is empty.
pub fn find_max_version(entries: &[IndexEntry]) -> String {
    // Prefer non-yanked stable or pre-release versions.
    entries
        .iter()
        .filter(|e| !e.yanked)
        .filter_map(|e| semver::Version::parse(&e.vers).ok().map(|sv| (sv, &e.vers)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, s)| s.clone())
        // Fall back to all versions (including yanked) if everything is yanked.
        .or_else(|| {
            entries
                .iter()
                .filter_map(|e| semver::Version::parse(&e.vers).ok().map(|sv| (sv, &e.vers)))
                .max_by(|(a, _), (b, _)| a.cmp(b))
                .map(|(_, s)| s.clone())
        })
        .unwrap_or_default()
}

/// Find the highest non-yanked **stable** (no pre-release segment) version.
pub fn find_max_stable_version(entries: &[IndexEntry]) -> Option<String> {
    entries
        .iter()
        .filter(|e| !e.yanked)
        .filter_map(|e| semver::Version::parse(&e.vers).ok().map(|sv| (sv, &e.vers)))
        .filter(|(sv, _)| sv.pre.is_empty())
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, s)| s.clone())
}

// ---------------------------------------------------------------------------
// IndexEntry → Version
// ---------------------------------------------------------------------------

/// Convert a sparse index [`IndexEntry`] into a public [`Version`].
///
/// Fields not available in the index (license, downloads, timestamps, …) are
/// set to `None` / `0` / epoch defaults.
#[allow(deprecated)] // VersionLinks::authors is deprecated but must be set
pub fn entry_to_version(entry: &IndexEntry) -> Version {
    let id = synthesize_id(&entry.name, &entry.vers);
    Version {
        crate_name: entry.name.clone(),
        created_at: epoch(),
        updated_at: epoch(),
        dl_path: format!("/api/v1/crates/{}/{}/download", entry.name, entry.vers),
        downloads: 0,
        features: entry.merged_features(),
        id,
        num: entry.vers.clone(),
        yanked: entry.yanked,
        license: None,
        readme_path: None,
        links: VersionLinks {
            // Deprecated; always empty.
            authors: String::new(),
            dependencies: format!(
                "/api/v1/crates/{}/{}/dependencies",
                entry.name, entry.vers
            ),
            version_downloads: format!(
                "/api/v1/crates/{}/{}/downloads",
                entry.name, entry.vers
            ),
        },
        crate_size: None,
        published_by: None,
    }
}

// ---------------------------------------------------------------------------
// IndexDep → Dependency
// ---------------------------------------------------------------------------

/// Convert a sparse index [`IndexDep`] into a public [`Dependency`].
///
/// The `version_id` parameter should be the synthetic ID of the parent
/// version so that `Dependency::version_id` is correctly populated.
///
/// When a dependency is renamed (has a `package` field), `crate_id` is set to
/// the **actual** crate name (`package`), not the local rename (`name`).
pub fn index_dep_to_dependency(dep: &IndexDep, version_id: u64) -> Dependency {
    // The actual published crate name is in `package` when the dep is renamed.
    let crate_id = dep.package.as_deref().unwrap_or(&dep.name).to_string();
    Dependency {
        id: synthesize_id(&crate_id, &dep.req),
        crate_id,
        default_features: dep.default_features,
        downloads: 0,
        features: dep.features.clone(),
        kind: dep.kind.as_deref().unwrap_or("normal").to_string(),
        optional: dep.optional,
        req: dep.req.clone(),
        target: dep.target.clone(),
        version_id,
    }
}

// ---------------------------------------------------------------------------
// [IndexEntry] → Crate
// ---------------------------------------------------------------------------

/// Build a [`Crate`] struct from a slice of index entries for one crate.
///
/// The slice must be non-empty.  Metadata not available in the index
/// (description, homepage, repository, download counts, …) is set to
/// `None` / `0`.
#[allow(deprecated)] // Crate::license is deprecated but must be initialised
pub fn entries_to_crate(crate_name: &str, entries: &[IndexEntry]) -> Crate {
    let max_version = find_max_version(entries);
    let max_stable_version = find_max_stable_version(entries);
    // Use the canonical name stored in the index (preserves original casing).
    let canonical_name = entries
        .first()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| crate_name.to_string());
    Crate {
        id: canonical_name.clone(),
        name: canonical_name.clone(),
        description: None,
        license: None,
        documentation: None,
        homepage: None,
        repository: None,
        downloads: 0,
        recent_downloads: None,
        // The index does not carry category/keyword data; callers may enrich
        // the struct from other sources if needed.
        categories: Some(vec![]),
        keywords: Some(vec![]),
        versions: Some(
            entries
                .iter()
                .map(|e| synthesize_id(&e.name, &e.vers))
                .collect(),
        ),
        max_version,
        max_stable_version,
        links: CrateLinks {
            owner_team: format!("/api/v1/crates/{}/owner_team", canonical_name),
            owner_user: format!("/api/v1/crates/{}/owner_user", canonical_name),
            owners: format!("/api/v1/crates/{}/owners", canonical_name),
            reverse_dependencies: format!(
                "/api/v1/crates/{}/reverse_dependencies",
                canonical_name
            ),
            version_downloads: format!("/api/v1/crates/{}/downloads", canonical_name),
            versions: Some(format!("/api/v1/crates/{}/versions", canonical_name)),
        },
        created_at: epoch(),
        updated_at: epoch(),
        exact_match: Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(name: &str, vers: &str, yanked: bool) -> IndexEntry {
        IndexEntry {
            name: name.to_string(),
            vers: vers.to_string(),
            deps: vec![],
            cksum: String::new(),
            features: Default::default(),
            yanked,
            links: None,
            v: 1,
            features2: Default::default(),
        }
    }

    #[test]
    fn synthesize_id_stable() {
        assert_eq!(synthesize_id("serde", "1.0.0"), synthesize_id("serde", "1.0.0"));
        assert_ne!(synthesize_id("serde", "1.0.0"), synthesize_id("serde", "1.0.1"));
        assert_ne!(synthesize_id("ab", "cd"), synthesize_id("a", "bcd"));
    }

    #[test]
    fn find_max_version_skips_yanked() {
        let entries = vec![
            make_entry("foo", "0.1.0", false),
            make_entry("foo", "0.2.0", true),
            make_entry("foo", "0.3.0", false),
        ];
        assert_eq!(find_max_version(&entries), "0.3.0");
    }

    #[test]
    fn find_max_version_all_yanked_falls_back() {
        let entries = vec![
            make_entry("foo", "0.1.0", true),
            make_entry("foo", "0.2.0", true),
        ];
        assert_eq!(find_max_version(&entries), "0.2.0");
    }

    #[test]
    fn find_max_stable_version_excludes_prerelease() {
        let entries = vec![
            make_entry("foo", "1.0.0", false),
            make_entry("foo", "1.1.0-alpha.1", false),
        ];
        assert_eq!(find_max_stable_version(&entries), Some("1.0.0".to_string()));
    }

    #[test]
    fn entry_to_version_fields() {
        let entry = make_entry("serde", "1.0.193", false);
        let v = entry_to_version(&entry);
        assert_eq!(v.crate_name, "serde");
        assert_eq!(v.num, "1.0.193");
        assert!(!v.yanked);
        assert_eq!(v.downloads, 0);
        assert!(v.license.is_none());
        assert_eq!(
            v.dl_path,
            "/api/v1/crates/serde/1.0.193/download"
        );
    }

    #[test]
    fn index_dep_to_dependency_renamed() {
        use crate::index::IndexDep;
        let dep = IndexDep {
            name: "rand_alias".to_string(),
            req: "^0.8".to_string(),
            features: vec![],
            default_features: true,
            optional: false,
            target: None,
            kind: Some("normal".to_string()),
            registry: None,
            package: Some("rand".to_string()),
        };
        let d = index_dep_to_dependency(&dep, 42);
        // crate_id should be the actual package name, not the alias.
        assert_eq!(d.crate_id, "rand");
        assert_eq!(d.req, "^0.8");
        assert_eq!(d.version_id, 42);
    }
}
