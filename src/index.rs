//! Sparse Registry Index utilities.
//!
//! The crates.io sparse index is available at `https://index.crates.io/` and
//! follows the same path layout as a git-based cargo registry index:
//!
//! | Name length | URL path                        |
//! |-------------|----------------------------------|
//! | 1           | `1/{name}`                       |
//! | 2           | `2/{name}`                       |
//! | 3           | `3/{first_char}/{name}`          |
//! | ≥ 4         | `{first2}/{next2}/{name}`        |
//!
//! Each file is newline-delimited JSON (ndjson): one JSON object per line,
//! one line per published version of that crate (append-only log).
//!
//! ## Conversion rationale
//!
//! The sparse index entry contains a subset of the fields exposed by the
//! crates.io web API.  The table below documents how each field of the
//! public types is sourced:
//!
//! ### `CrateResponse` / `Crate`
//! | Target field       | Source                                          |
//! |--------------------|------------------------------------------------|
//! | id / name          | `IndexEntry::name`                             |
//! | max_version        | highest non-yanked semver across all entries   |
//! | max_stable_version | highest non-yanked stable semver               |
//! | versions (ids)     | sequential 1-based position in the ndjson file |
//! | description        | not available → `None`                         |
//! | downloads          | not available → `0`                            |
//! | created_at/updated_at | not available → current UTC time           |
//! | links              | synthesised from crate name                    |
//! | exact_match        | always `true` (queried by exact name)          |
//!
//! ### `Version`
//! | Target field   | Source                              |
//! |----------------|-------------------------------------|
//! | num            | `IndexEntry::vers`                  |
//! | yanked         | `IndexEntry::yanked`                |
//! | features       | `IndexEntry::features` + `features2`|
//! | dl_path        | synthesised standard crates.io path |
//! | id             | 1-based position in the ndjson file |
//! | license        | not available → `None`              |
//! | downloads      | not available → `0`                 |
//! | created_at/updated_at | not available → epoch        |
//!
//! ### `Dependency`
//! | Target field    | Source                                             |
//! |-----------------|---------------------------------------------------|
//! | crate_id        | `IndexDep::package` if set, else `IndexDep::name` |
//! | req             | `IndexDep::req`                                   |
//! | features        | `IndexDep::features`                              |
//! | optional        | `IndexDep::optional`                              |
//! | default_features| `IndexDep::default_features`                      |
//! | target          | `IndexDep::target`                                |
//! | kind            | `IndexDep::kind` (default "normal")               |
//! | downloads/id/version_id | not available → `0`                     |

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use serde_derive::Deserialize;

use crate::{
    error::{Error, NotFoundError},
    types::*,
};

// ── Sparse-index wire types ──────────────────────────────────────────────────

/// One line of the sparse-index ndjson file; represents one published version.
#[derive(Deserialize, Debug)]
pub(crate) struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDep>,
    /// Primary feature map.
    #[serde(default)]
    pub features: HashMap<String, Vec<String>>,
    /// Extended feature map (format version 2).
    #[serde(default)]
    pub features2: HashMap<String, Vec<String>>,
    pub yanked: bool,
}

/// Dependency entry inside an [`IndexEntry`].
#[derive(Deserialize, Debug)]
pub(crate) struct IndexDep {
    /// The dependency's crate name as it appears in the index.
    pub name: String,
    /// The version requirement (semver req string).
    pub req: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "bool_true")]
    pub default_features: bool,
    pub target: Option<String>,
    /// "normal", "dev", or "build".
    pub kind: Option<String>,
    /// If set, the dependency resolves from a different registry.
    pub registry: Option<String>,
    /// Actual crate name if the dependency was renamed with `package = "..."`.
    pub package: Option<String>,
}

fn bool_true() -> bool {
    true
}

// ── URL helpers ──────────────────────────────────────────────────────────────

/// Base URL for the crates.io sparse registry index.
pub(crate) const SPARSE_INDEX_BASE: &str = "https://index.crates.io/";

/// Compute the sparse-index URL path component for `crate_name`.
///
/// The name is lowercased before computing the path, matching cargo's behaviour.
pub(crate) fn index_path(crate_name: &str) -> String {
    let name = crate_name.to_lowercase();
    match name.len() {
        0 => name,
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => {
            let first = &name[..1];
            format!("3/{first}/{name}")
        }
        _ => {
            let first2 = &name[..2];
            let next2 = &name[2..4];
            format!("{first2}/{next2}/{name}")
        }
    }
}

/// Return the full sparse-index URL for `crate_name`, or an [`Error::NotFound`]
/// if the name contains a `/` (which the index does not support).
pub(crate) fn build_index_url(crate_name: &str) -> Result<url::Url, Error> {
    if crate_name.contains('/') {
        return Err(Error::NotFound(NotFoundError {
            url: format!("{SPARSE_INDEX_BASE}{}", index_path(crate_name)),
        }));
    }
    let url = format!("{SPARSE_INDEX_BASE}{}", index_path(crate_name));
    url::Url::parse(&url).map_err(Error::from)
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse an ndjson sparse-index response body into a list of [`IndexEntry`] values.
///
/// Lines that are empty or that fail to deserialise (e.g. future-format lines)
/// are silently skipped so that the parser remains forward-compatible.
pub(crate) fn parse_index_entries(body: &str) -> Vec<IndexEntry> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// ── Conversion ───────────────────────────────────────────────────────────────

/// Convert a slice of sparse-index entries into a [`CrateResponse`].
///
/// Fields that are not available in the sparse index (downloads, authors,
/// owners, timestamps, etc.) are filled with safe zero/None/empty defaults so
/// that the returned value is always a valid [`CrateResponse`] that
/// downstream code can pattern-match on without special cases.
pub(crate) fn entries_to_crate_response(
    queried_name: &str,
    entries: &[IndexEntry],
) -> CrateResponse {
    let now: DateTime<Utc> = Utc::now();
    // Use 1970-01-01 as a sentinel "unknown" timestamp for per-version data.
    let epoch: DateTime<Utc> = Utc.timestamp_opt(0, 0).single().unwrap_or(now);

    // The canonical display name comes from the first entry's `name` field
    // (preserving the original casing published by the author).
    let display_name = entries
        .first()
        .map(|e| e.name.as_str())
        .unwrap_or(queried_name)
        .to_string();

    // ── Determine max_version and max_stable_version ─────────────────────────
    // Filter out yanked entries first; if *all* are yanked fall back to yanked.
    let non_yanked: Vec<&IndexEntry> = entries.iter().filter(|e| !e.yanked).collect();
    let candidates = if non_yanked.is_empty() {
        entries.iter().collect::<Vec<_>>()
    } else {
        non_yanked
    };

    let max_version = candidates
        .iter()
        .filter_map(|e| {
            semver::Version::parse(&e.vers)
                .ok()
                .map(|v| (v, e.vers.clone()))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, s)| s)
        .unwrap_or_default();

    // Stable = no pre-release segment.
    let max_stable_version = candidates
        .iter()
        .filter_map(|e| {
            semver::Version::parse(&e.vers).ok().and_then(|v| {
                if v.pre.is_empty() {
                    Some((v, e.vers.clone()))
                } else {
                    None
                }
            })
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, s)| s);

    // ── Build Version list ────────────────────────────────────────────────────
    // Versions are assigned sequential 1-based IDs matching their position in
    // the ndjson file (oldest → newest, ascending order).
    let versions: Vec<Version> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let id = (i + 1) as u64;

            // Merge features and features2 (format v2 extension).
            let mut merged_features = e.features.clone();
            for (k, vals) in &e.features2 {
                merged_features
                    .entry(k.clone())
                    .or_default()
                    .extend(vals.clone());
            }

            #[allow(deprecated)]
            Version {
                crate_name: display_name.clone(),
                created_at: epoch,
                updated_at: epoch,
                dl_path: format!(
                    "/api/v1/crates/{}/{}/download",
                    display_name, e.vers
                ),
                downloads: 0,
                features: merged_features,
                id,
                num: e.vers.clone(),
                yanked: e.yanked,
                license: None,
                readme_path: None,
                links: VersionLinks {
                    authors: String::new(),
                    dependencies: format!(
                        "/api/v1/crates/{}/{}/dependencies",
                        display_name, e.vers
                    ),
                    version_downloads: format!(
                        "/api/v1/crates/{}/{}/downloads",
                        display_name, e.vers
                    ),
                },
                crate_size: None,
                published_by: None,
            }
        })
        .collect();

    let version_ids: Vec<u64> = versions.iter().map(|v| v.id).collect();

    #[allow(deprecated)]
    let crate_data = Crate {
        id: display_name.clone(),
        name: display_name.clone(),
        description: None,
        license: None,
        documentation: None,
        homepage: None,
        repository: None,
        downloads: 0,
        recent_downloads: None,
        categories: Some(vec![]),
        keywords: Some(vec![]),
        versions: Some(version_ids),
        max_version,
        max_stable_version,
        links: CrateLinks {
            owner_team: format!("/api/v1/crates/{display_name}/owner_team"),
            owner_user: format!("/api/v1/crates/{display_name}/owner_user"),
            owners: format!("/api/v1/crates/{display_name}/owners"),
            reverse_dependencies: format!(
                "/api/v1/crates/{display_name}/reverse_dependencies"
            ),
            version_downloads: format!("/api/v1/crates/{display_name}/downloads"),
            versions: None,
        },
        created_at: now,
        updated_at: now,
        exact_match: Some(true),
    };

    CrateResponse {
        categories: vec![],
        crate_data,
        keywords: vec![],
        versions,
    }
}

/// Convert the [`IndexDep`] entries for the requested `version` into the public
/// [`Dependency`] type.  Returns `None` when the requested version is not found.
pub(crate) fn entries_to_dependencies(
    version: &str,
    entries: &[IndexEntry],
) -> Option<Vec<Dependency>> {
    entries
        .iter()
        .find(|e| e.vers == version)
        .map(|e| index_deps_to_dependencies(&e.deps))
}

fn index_deps_to_dependencies(deps: &[IndexDep]) -> Vec<Dependency> {
    deps.iter()
        .enumerate()
        .map(|(i, d)| {
            // When a dependency is renamed, `package` holds the real crate name.
            let crate_id = d
                .package
                .as_deref()
                .unwrap_or(d.name.as_str())
                .to_string();

            Dependency {
                crate_id,
                default_features: d.default_features,
                downloads: 0,
                features: d.features.clone(),
                id: (i + 1) as u64,
                kind: d.kind.as_deref().unwrap_or("normal").to_string(),
                optional: d.optional,
                req: d.req.clone(),
                target: d.target.clone(),
                version_id: 0,
            }
        })
        .collect()
}

/// Build an empty [`CrateDownloads`] (download stats are not available via the
/// sparse index).
pub(crate) fn empty_crate_downloads() -> CrateDownloads {
    CrateDownloads {
        version_downloads: vec![],
        meta: CrateDownloadsMeta {
            extra_downloads: vec![],
        },
    }
}

/// Build an empty [`ReverseDependencies`] (reverse-dep data is not available
/// via the sparse index without enumerating the entire index).
pub(crate) fn empty_reverse_dependencies() -> ReverseDependencies {
    ReverseDependencies {
        dependencies: vec![],
        meta: Meta { total: 0 },
    }
}

/// Build an [`Error`] indicating that an operation is not supported by the
/// sparse-index backend.
pub(crate) fn not_supported(operation: &str) -> Error {
    Error::Api(ApiErrors {
        errors: vec![ApiError {
            detail: Some(format!(
                "Operation '{operation}' is not available via the sparse registry index. \
                 Use get_crate() or crate_dependencies() instead."
            )),
        }],
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_path_1() {
        assert_eq!(index_path("a"), "1/a");
    }

    #[test]
    fn test_index_path_2() {
        assert_eq!(index_path("ab"), "2/ab");
    }

    #[test]
    fn test_index_path_3() {
        assert_eq!(index_path("abc"), "3/a/abc");
    }

    #[test]
    fn test_index_path_4plus() {
        assert_eq!(index_path("serde"), "se/rd/serde");
        assert_eq!(index_path("tokio"), "to/ki/tokio");
        assert_eq!(index_path("crates_io_api"), "cr/at/crates_io_api");
    }

    #[test]
    fn test_index_path_uppercase_normalised() {
        assert_eq!(index_path("Serde"), "se/rd/serde");
    }

    #[test]
    fn test_build_index_url_rejects_slash() {
        assert!(matches!(build_index_url("a/b"), Err(Error::NotFound(_))));
    }

    #[test]
    fn test_parse_index_entries_basic() {
        let ndjson = r#"{"name":"foo","vers":"0.1.0","deps":[],"cksum":"abc","features":{},"yanked":false}
{"name":"foo","vers":"0.2.0","deps":[],"cksum":"def","features":{},"yanked":false}
"#;
        let entries = parse_index_entries(ndjson);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].vers, "0.1.0");
        assert_eq!(entries[1].vers, "0.2.0");
    }

    #[test]
    fn test_parse_index_entries_skips_invalid_lines() {
        let ndjson = "not-json\n{\"name\":\"foo\",\"vers\":\"1.0.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n";
        let entries = parse_index_entries(ndjson);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_entries_to_crate_response_max_version() {
        let ndjson = concat!(
            "{\"name\":\"mylib\",\"vers\":\"0.1.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
            "{\"name\":\"mylib\",\"vers\":\"1.0.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
            "{\"name\":\"mylib\",\"vers\":\"2.0.0-alpha.1\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
        );
        let entries = parse_index_entries(ndjson);
        let resp = entries_to_crate_response("mylib", &entries);

        // max_version should be the alpha because 2.0.0-alpha.1 > 1.0.0 in semver
        assert_eq!(resp.crate_data.max_version, "2.0.0-alpha.1");
        // max_stable_version should be 1.0.0 (no pre-release)
        assert_eq!(
            resp.crate_data.max_stable_version,
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn test_entries_to_crate_response_yanked_fallback() {
        let ndjson = concat!(
            "{\"name\":\"mylib\",\"vers\":\"1.0.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":true}\n",
            "{\"name\":\"mylib\",\"vers\":\"0.9.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":true}\n",
        );
        let entries = parse_index_entries(ndjson);
        let resp = entries_to_crate_response("mylib", &entries);
        // All yanked: use highest among yanked
        assert_eq!(resp.crate_data.max_version, "1.0.0");
    }

    #[test]
    fn test_entries_to_crate_response_version_ids_sequential() {
        let ndjson = concat!(
            "{\"name\":\"foo\",\"vers\":\"0.1.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
            "{\"name\":\"foo\",\"vers\":\"0.2.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
        );
        let entries = parse_index_entries(ndjson);
        let resp = entries_to_crate_response("foo", &entries);
        assert_eq!(resp.versions[0].id, 1);
        assert_eq!(resp.versions[1].id, 2);
        assert_eq!(resp.versions[0].num, "0.1.0");
        assert_eq!(resp.versions[1].num, "0.2.0");
    }

    #[test]
    fn test_entries_to_dependencies_found() {
        let ndjson = concat!(
            "{\"name\":\"foo\",\"vers\":\"1.0.0\",\"deps\":[",
            "{\"name\":\"serde\",\"req\":\"^1\",\"features\":[],\"optional\":false,\"default_features\":true,\"target\":null,\"kind\":\"normal\"}",
            "],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
        );
        let entries = parse_index_entries(ndjson);
        let deps = entries_to_dependencies("1.0.0", &entries).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].crate_id, "serde");
        assert_eq!(deps[0].req, "^1");
        assert_eq!(deps[0].kind, "normal");
    }

    #[test]
    fn test_entries_to_dependencies_renamed_dep() {
        let ndjson = concat!(
            "{\"name\":\"foo\",\"vers\":\"1.0.0\",\"deps\":[",
            "{\"name\":\"rand_alias\",\"req\":\"^0.8\",\"features\":[],\"optional\":false,\"default_features\":true,\"target\":null,\"kind\":\"normal\",\"package\":\"rand\"}",
            "],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n",
        );
        let entries = parse_index_entries(ndjson);
        let deps = entries_to_dependencies("1.0.0", &entries).unwrap();
        // crate_id should use `package` (the real crate name)
        assert_eq!(deps[0].crate_id, "rand");
    }

    #[test]
    fn test_entries_to_dependencies_version_not_found() {
        let ndjson = "{\"name\":\"foo\",\"vers\":\"1.0.0\",\"deps\":[],\"cksum\":\"\",\"features\":{},\"yanked\":false}\n";
        let entries = parse_index_entries(ndjson);
        assert!(entries_to_dependencies("2.0.0", &entries).is_none());
    }

    #[test]
    fn test_features2_merged() {
        let ndjson = concat!(
            "{\"name\":\"foo\",\"vers\":\"1.0.0\",\"deps\":[],\"cksum\":\"\",",
            "\"features\":{\"default\":[\"std\"]},",
            "\"features2\":{\"async\":[\"dep:tokio\"]},",
            "\"yanked\":false}\n",
        );
        let entries = parse_index_entries(ndjson);
        let resp = entries_to_crate_response("foo", &entries);
        let feats = &resp.versions[0].features;
        assert!(feats.contains_key("default"));
        assert!(feats.contains_key("async"));
    }
}
