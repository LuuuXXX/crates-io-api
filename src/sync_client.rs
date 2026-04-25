//! Synchronous client backed by the crates.io sparse registry index.

use log::trace;
use reqwest::{blocking::Client as HttpClient, header, StatusCode};

use crate::{
    error::{Error, NotFoundError, PermissionDeniedError},
    index::{
        build_deps_map, build_index_url, empty_crate_downloads, empty_reverse_dependencies,
        entries_to_crate_response, entries_to_dependencies, not_supported, parse_index_entries,
    },
    types::*,
};

/// A synchronous client for the crates.io sparse registry index.
///
/// This client exposes the same interface as the original `crates_io_api`
/// crate but fetches metadata from the [crates.io sparse index]
/// (`https://index.crates.io/`) rather than the JSON web API.
///
/// # What is available
///
/// | Method                         | Availability                                  |
/// |-------------------------------|-----------------------------------------------|
/// | [`get_crate`]                  | ✅ full                                       |
/// | [`crate_dependencies`]         | ✅ full                                       |
/// | [`crate_downloads`]            | ⚠️ returns empty (not in sparse index)        |
/// | [`crate_owners`]               | ⚠️ returns empty (not in sparse index)        |
/// | [`crate_authors`]              | ⚠️ returns empty (not in sparse index)        |
/// | [`crate_reverse_dependencies`] | ⚠️ returns empty (not in sparse index)        |
/// | [`full_crate`]                 | ✅ partial (no downloads/owners/rev-deps)     |
/// | [`summary`]                    | ❌ unsupported – returns `Error::Api`         |
/// | [`crates`]                     | ❌ unsupported – returns `Error::Api`         |
/// | [`user`]                       | ❌ unsupported – returns `Error::Api`         |
///
/// [`get_crate`]: SyncClient::get_crate
/// [`crate_dependencies`]: SyncClient::crate_dependencies
/// [`crate_downloads`]: SyncClient::crate_downloads
/// [`crate_owners`]: SyncClient::crate_owners
/// [`crate_authors`]: SyncClient::crate_authors
/// [`crate_reverse_dependencies`]: SyncClient::crate_reverse_dependencies
/// [`full_crate`]: SyncClient::full_crate
/// [`summary`]: SyncClient::summary
/// [`crates`]: SyncClient::crates
/// [`user`]: SyncClient::user
pub struct SyncClient {
    client: HttpClient,
    rate_limit: std::time::Duration,
    last_request_time: std::sync::Mutex<Option<std::time::Instant>>,
}

impl SyncClient {
    /// Instantiate a new client.
    ///
    /// Returns an error if the given user agent string is not a valid HTTP
    /// header value.
    ///
    /// `rate_limit` controls the minimum interval between successive HTTP
    /// requests.  At most one request is issued per `rate_limit` duration.
    /// Pass `Duration::ZERO` to disable throttling.
    ///
    /// # Example
    ///
    /// ```rust
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = crates_io_api::SyncClient::new(
    ///     "my-bot (contact@example.com)",
    ///     std::time::Duration::from_millis(1000),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(
        user_agent: &str,
        rate_limit: std::time::Duration,
    ) -> Result<Self, reqwest::header::InvalidHeaderValue> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(user_agent)?,
        );

        let client = HttpClient::builder()
            .default_headers(headers)
            .build()
            .expect("failed to build reqwest blocking client");

        Ok(Self {
            client,
            rate_limit,
            last_request_time: std::sync::Mutex::new(None),
        })
    }

    // ── Internal HTTP helper ─────────────────────────────────────────────────

    /// Fetch the raw text body at `url`, honouring the configured rate limit.
    fn fetch_text(&self, url: url::Url) -> Result<String, Error> {
        trace!("GET {}", url);

        // Rate limiting: sleep if the previous request was too recent.
        let mut lock = self.last_request_time.lock().expect("mutex poisoned");
        if let Some(last) = *lock {
            let elapsed = last.elapsed();
            if elapsed < self.rate_limit {
                std::thread::sleep(self.rate_limit - elapsed);
            }
        }

        let request_start = std::time::Instant::now();
        let res = self.client.get(url.clone()).send()?;

        if !res.status().is_success() {
            return Err(match res.status() {
                StatusCode::NOT_FOUND => Error::NotFound(NotFoundError {
                    url: url.to_string(),
                }),
                StatusCode::FORBIDDEN => {
                    let reason = res.text().unwrap_or_default();
                    Error::PermissionDenied(PermissionDeniedError { reason })
                }
                _ => Error::from(res.error_for_status().unwrap_err()),
            });
        }

        *lock = Some(request_start);
        Ok(res.text()?)
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Retrieve information about a crate from the sparse registry index.
    ///
    /// Returns [`Error::NotFound`] when the crate does not exist in the index.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let resp = crates_io_api::SyncClient::new(
    ///     "my-bot (contact@example.com)",
    ///     std::time::Duration::from_millis(1000),
    /// )?
    /// .get_crate("serde")?;
    /// println!("max version: {}", resp.crate_data.max_version);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_crate(&self, crate_name: &str) -> Result<CrateResponse, Error> {
        let url = build_index_url(crate_name)?;
        let body = self.fetch_text(url)?;
        let entries = parse_index_entries(&body);
        if entries.is_empty() {
            return Err(Error::NotFound(NotFoundError {
                url: format!(
                    "{}{}",
                    crate::index::SPARSE_INDEX_BASE,
                    crate::index::index_path(crate_name)
                ),
            }));
        }
        Ok(entries_to_crate_response(crate_name, &entries))
    }

    /// Retrieve the dependencies of a specific crate version from the sparse index.
    pub fn crate_dependencies(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Vec<Dependency>, Error> {
        let url = build_index_url(crate_name)?;
        let url_str = url.to_string();
        let body = self.fetch_text(url)?;
        let entries = parse_index_entries(&body);
        entries_to_dependencies(version, &entries).ok_or_else(|| {
            Error::NotFound(NotFoundError { url: url_str })
        })
    }

    /// Retrieve download statistics for a crate.
    ///
    /// **Note:** Download statistics are not available via the sparse registry
    /// index.  This method always returns an empty [`CrateDownloads`].
    pub fn crate_downloads(&self, _crate_name: &str) -> Result<CrateDownloads, Error> {
        Ok(empty_crate_downloads())
    }

    /// Retrieve the owners of a crate.
    ///
    /// **Note:** Owner information is not available via the sparse registry
    /// index.  This method always returns an empty list.
    pub fn crate_owners(&self, _crate_name: &str) -> Result<Vec<User>, Error> {
        Ok(vec![])
    }

    /// Get a single page of reverse dependencies.
    ///
    /// **Note:** Reverse dependency data is not available via the sparse
    /// registry index.  This method always returns an empty page.
    pub fn crate_reverse_dependencies_page(
        &self,
        _crate_name: &str,
        _page: u64,
    ) -> Result<ReverseDependencies, Error> {
        Ok(empty_reverse_dependencies())
    }

    /// Load all reverse dependencies of a crate.
    ///
    /// **Note:** Reverse dependency data is not available via the sparse
    /// registry index.  This method always returns an empty result.
    pub fn crate_reverse_dependencies(
        &self,
        _crate_name: &str,
    ) -> Result<ReverseDependencies, Error> {
        Ok(empty_reverse_dependencies())
    }

    /// Get the total count of reverse dependencies.
    ///
    /// **Note:** Reverse dependency data is not available via the sparse
    /// registry index.  This method always returns `0`.
    pub fn crate_reverse_dependency_count(&self, _crate_name: &str) -> Result<u64, Error> {
        Ok(0)
    }

    /// Retrieve the authors for a crate version.
    ///
    /// **Note:** Author information is not available via the sparse registry
    /// index.  This method always returns an empty [`Authors`].
    pub fn crate_authors(&self, _crate_name: &str, _version: &str) -> Result<Authors, Error> {
        Ok(Authors { names: vec![] })
    }

    /// Retrieve all available information for a crate.
    ///
    /// Uses the sparse index for crate metadata and version dependency data.
    /// Fields not available in the sparse index (downloads, owners, authors,
    /// reverse dependencies) are returned as empty / zero.
    ///
    /// The `all_versions` flag controls whether per-version dependency data is
    /// included.  When `false`, only the latest version's dependencies are
    /// included. The current implementation performs a single sparse-index
    /// fetch to load all crate and dependency data for the selected versions.
    pub fn full_crate(&self, name: &str, all_versions: bool) -> Result<FullCrate, Error> {
        // Single fetch: build CrateResponse and dep entries in one round-trip.
        let url = build_index_url(name)?;
        let body = self.fetch_text(url)?;
        let entries = parse_index_entries(&body);
        if entries.is_empty() {
            return Err(Error::NotFound(NotFoundError {
                url: format!(
                    "{}{}",
                    crate::index::SPARSE_INDEX_BASE,
                    crate::index::index_path(name)
                ),
            }));
        }
        let resp = entries_to_crate_response(name, &entries);
        let data = &resp.crate_data;

        let versions_to_process: Vec<&Version> = if resp.versions.is_empty() {
            vec![]
        } else if all_versions {
            resp.versions.iter().collect()
        } else {
            // Versions are newest-first; the first entry is the latest.
            resp.versions.first().into_iter().collect()
        };

        // Precompute a version → deps map once so that FullVersion assembly is
        // O(n) instead of O(n²).
        let deps_map = build_deps_map(&entries);

        let full_versions: Vec<FullVersion> = versions_to_process
            .iter()
            .map(|v| {
                let deps = deps_map.get(v.num.as_str()).cloned().unwrap_or_default();
                #[allow(deprecated)]
                FullVersion {
                    created_at: v.created_at,
                    updated_at: v.updated_at,
                    dl_path: v.dl_path.clone(),
                    downloads: v.downloads,
                    features: v.features.clone(),
                    id: v.id,
                    num: v.num.clone(),
                    yanked: v.yanked,
                    license: v.license.clone(),
                    readme_path: v.readme_path.clone(),
                    links: VersionLinks {
                        authors: String::new(),
                        dependencies: v.links.dependencies.clone(),
                        version_downloads: v.links.version_downloads.clone(),
                    },
                    author_names: vec![],
                    dependencies: deps,
                }
            })
            .collect();

        // Versions are newest-first, so index 0 is the latest version.
        let license = full_versions.first().and_then(|v| v.license.clone());

        Ok(FullCrate {
            id: data.id.clone(),
            name: data.name.clone(),
            description: data.description.clone(),
            license,
            documentation: data.documentation.clone(),
            homepage: data.homepage.clone(),
            repository: data.repository.clone(),
            total_downloads: data.downloads,
            max_version: data.max_version.clone(),
            max_stable_version: data.max_stable_version.clone(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            categories: resp.categories,
            keywords: resp.keywords,
            downloads: empty_crate_downloads(),
            owners: vec![],
            reverse_dependencies: empty_reverse_dependencies(),
            versions: full_versions,
        })
    }

    /// Retrieve a page of crates.
    ///
    /// **Note:** Crate enumeration is not available via the sparse registry
    /// index.  This method always returns `Error::Api`.
    pub fn crates(&self, _query: CratesQuery) -> Result<CratesPage, Error> {
        Err(not_supported("crates"))
    }

    /// Retrieve a summary containing crates.io-wide statistics.
    ///
    /// **Note:** Summary statistics are not available via the sparse registry
    /// index.  This method always returns `Error::Api`.
    pub fn summary(&self) -> Result<Summary, Error> {
        Err(not_supported("summary"))
    }

    /// Retrieve a user by username.
    ///
    /// **Note:** User data is not available via the sparse registry index.
    /// This method always returns `Error::Api`.
    pub fn user(&self, _username: &str) -> Result<User, Error> {
        Err(not_supported("user"))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> SyncClient {
        SyncClient::new(
            "crates-io-api-test (github.com/LuuuXXX/crates-io-api)",
            std::time::Duration::from_millis(500),
        )
        .unwrap()
    }

    /// SyncClient must be Send so it can be used across threads.
    #[test]
    fn sync_client_is_send() {
        let client = test_client();
        let _: &dyn Send = &client;
    }

    #[test]
    fn new_rejects_invalid_user_agent() {
        // A header value containing a newline is always invalid.
        let result = SyncClient::new("bad\nagent", std::time::Duration::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn crate_downloads_returns_empty() {
        let client = test_client();
        let dl = client.crate_downloads("serde").unwrap();
        assert!(dl.version_downloads.is_empty());
    }

    #[test]
    fn crate_owners_returns_empty() {
        let client = test_client();
        let owners = client.crate_owners("serde").unwrap();
        assert!(owners.is_empty());
    }

    #[test]
    fn crate_authors_returns_empty() {
        let client = test_client();
        let authors = client.crate_authors("serde", "1.0.0").unwrap();
        assert!(authors.names.is_empty());
    }

    #[test]
    fn reverse_deps_returns_empty() {
        let client = test_client();
        let rdeps = client.crate_reverse_dependencies("serde").unwrap();
        assert!(rdeps.dependencies.is_empty());
        assert_eq!(rdeps.meta.total, 0);
    }

    #[test]
    fn reverse_dep_count_returns_zero() {
        let client = test_client();
        assert_eq!(client.crate_reverse_dependency_count("serde").unwrap(), 0);
    }

    #[test]
    fn summary_returns_error() {
        let client = test_client();
        assert!(matches!(client.summary(), Err(Error::Api(_))));
    }

    #[test]
    fn crates_returns_error() {
        let client = test_client();
        assert!(matches!(
            client.crates(CratesQuery::default()),
            Err(Error::Api(_))
        ));
    }

    #[test]
    fn user_returns_error() {
        let client = test_client();
        assert!(matches!(client.user("ferris"), Err(Error::Api(_))));
    }

    // ── Integration tests (require network) ──────────────────────────────────

    #[test]
    #[ignore]
    fn integration_get_crate_serde() {
        let client = test_client();
        let resp = client.get_crate("serde").unwrap();
        assert_eq!(resp.crate_data.name, "serde");
        assert!(!resp.versions.is_empty());
        // max_version must be a valid semver string
        semver::Version::parse(&resp.crate_data.max_version).unwrap();
    }

    #[test]
    #[ignore]
    fn integration_get_crate_not_found() {
        let client = test_client();
        let result = client.get_crate("this-crate-definitely-does-not-exist-xyz-42");
        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    #[ignore]
    fn integration_get_crate_with_slash_not_found() {
        let client = test_client();
        let result = client.get_crate("a/b");
        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    #[ignore]
    fn integration_crate_dependencies() {
        let client = test_client();
        // serde 1.0.0 has no dependencies
        let deps = client.crate_dependencies("serde", "1.0.0").unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    #[ignore]
    fn integration_full_crate() {
        let client = test_client();
        let full = client.full_crate("serde", false).unwrap();
        assert_eq!(full.name, "serde");
        assert!(!full.versions.is_empty());
    }
}
