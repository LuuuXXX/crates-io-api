//! Synchronous client backed by the crates.io sparse registry index.

use log::trace;
use reqwest::{blocking::Client as HttpClient, header, StatusCode, Url};

use super::Error;
use crate::convert::{
    entries_to_crate, entry_to_version, index_dep_to_dependency, synthesize_id,
};
use crate::error::{JsonDecodeError, NotFoundError, PermissionDeniedError};
use crate::index::{index_path, parse_index_file, IndexEntry};
use crate::types::*;

/// Base URL of the crates.io sparse registry index.
const INDEX_BASE: &str = "https://index.crates.io/";

/// Synchronous client for the crates.io **sparse registry index**.
///
/// This is the blocking counterpart of [`crate::AsyncClient`].  It offers the
/// same public API and the same availability guarantees — see
/// [`AsyncClient`](crate::AsyncClient) for a capability table.
///
/// The client is `Send` and can be shared across threads via `Arc<SyncClient>`.
pub struct SyncClient {
    client: HttpClient,
    base_url: Url,
    rate_limit: std::time::Duration,
    last_request_time: std::sync::Mutex<Option<std::time::Instant>>,
}

impl SyncClient {
    /// Create a new synchronous client.
    ///
    /// Returns an error if `user_agent` contains invalid header characters.
    ///
    /// ```rust
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = crates_io_api::SyncClient::new(
    ///     "my_bot (help@my_bot.com)",
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
        Ok(Self {
            client: HttpClient::builder()
                .default_headers(headers)
                .build()
                .unwrap(),
            base_url: Url::parse(INDEX_BASE).expect("static base URL is valid"),
            rate_limit,
            last_request_time: std::sync::Mutex::new(None),
        })
    }

    // -----------------------------------------------------------------------
    // Internal HTTP helper
    // -----------------------------------------------------------------------

    /// Perform a rate-limited GET request and return the response body as text.
    fn get_text(&self, url: &Url) -> Result<String, Error> {
        trace!("GET {}", url);

        let mut lock = self.last_request_time.lock().unwrap();
        if let Some(last) = lock.take() {
            let now = std::time::Instant::now();
            if last.elapsed() < self.rate_limit {
                std::thread::sleep((last + self.rate_limit) - now);
            }
        }

        let time = std::time::Instant::now();
        let res = self.client.get(url.clone()).send()?;

        if !res.status().is_success() {
            let err = match res.status() {
                StatusCode::NOT_FOUND => Error::NotFound(NotFoundError {
                    url: url.to_string(),
                }),
                StatusCode::FORBIDDEN => {
                    let reason = res.text().unwrap_or_default();
                    Error::PermissionDenied(PermissionDeniedError { reason })
                }
                _ => Error::from(res.error_for_status().unwrap_err()),
            };
            return Err(err);
        }

        *lock = Some(time);
        res.text().map_err(Error::from)
    }

    // -----------------------------------------------------------------------
    // Index fetch
    // -----------------------------------------------------------------------

    /// Fetch and parse all version entries for `crate_name` from the index.
    fn get_index_entries(&self, crate_name: &str) -> Result<Vec<IndexEntry>, Error> {
        if crate_name.contains('/') {
            return Err(Error::NotFound(NotFoundError {
                url: format!("{}{}", INDEX_BASE, index_path(crate_name)),
            }));
        }
        let path = index_path(crate_name);
        if path.is_empty() {
            return Err(Error::NotFound(NotFoundError {
                url: INDEX_BASE.to_string(),
            }));
        }
        let url = self.base_url.join(&path).map_err(Error::from)?;
        let content = self.get_text(&url)?;
        parse_index_file(&content).map_err(|e| {
            Error::JsonDecode(JsonDecodeError {
                message: format!("Failed to parse index entry for '{}': {}", crate_name, e),
            })
        })
    }

    // -----------------------------------------------------------------------
    // Public API (same signatures as base crates_io_api::SyncClient)
    // -----------------------------------------------------------------------

    /// Retrieve a summary of crates.io statistics.
    ///
    /// **Note:** Always returns an empty [`Summary`] — see
    /// [`AsyncClient::summary`](crate::AsyncClient::summary).
    pub fn summary(&self) -> Result<Summary, Error> {
        Ok(Summary {
            just_updated: vec![],
            most_downloaded: vec![],
            new_crates: vec![],
            most_recently_downloaded: vec![],
            num_crates: 0,
            num_downloads: 0,
            popular_categories: vec![],
            popular_keywords: vec![],
        })
    }

    /// Retrieve version and dependency information for a crate by name.
    pub fn get_crate(&self, crate_name: &str) -> Result<CrateResponse, Error> {
        let entries = self.get_index_entries(crate_name)?;
        if entries.is_empty() {
            return Err(Error::NotFound(NotFoundError {
                url: format!("{}{}", INDEX_BASE, index_path(crate_name)),
            }));
        }
        let crate_data = entries_to_crate(crate_name, &entries);
        let versions: Vec<Version> = entries.iter().map(entry_to_version).collect();
        Ok(CrateResponse {
            categories: vec![],
            crate_data,
            keywords: vec![],
            versions,
        })
    }

    /// Retrieve download statistics for a crate.
    ///
    /// **Note:** Always returns empty — see
    /// [`AsyncClient::crate_downloads`](crate::AsyncClient::crate_downloads).
    pub fn crate_downloads(&self, _crate_name: &str) -> Result<CrateDownloads, Error> {
        Ok(CrateDownloads {
            version_downloads: vec![],
            meta: CrateDownloadsMeta {
                extra_downloads: vec![],
            },
        })
    }

    /// Retrieve the owners of a crate.
    ///
    /// **Note:** Always returns an empty list — see
    /// [`AsyncClient::crate_owners`](crate::AsyncClient::crate_owners).
    pub fn crate_owners(&self, _crate_name: &str) -> Result<Vec<User>, Error> {
        Ok(vec![])
    }

    /// Retrieve a single page of reverse dependencies.
    ///
    /// **Note:** Always returns empty — see
    /// [`AsyncClient::crate_reverse_dependencies_page`](crate::AsyncClient::crate_reverse_dependencies_page).
    pub fn crate_reverse_dependencies_page(
        &self,
        _crate_name: &str,
        _page: u64,
    ) -> Result<ReverseDependencies, Error> {
        Ok(ReverseDependencies {
            dependencies: vec![],
            meta: Meta { total: 0 },
        })
    }

    /// Retrieve all reverse dependencies of a crate.
    ///
    /// **Note:** Always returns empty — see
    /// [`AsyncClient::crate_reverse_dependencies`](crate::AsyncClient::crate_reverse_dependencies).
    pub fn crate_reverse_dependencies(
        &self,
        crate_name: &str,
    ) -> Result<ReverseDependencies, Error> {
        self.crate_reverse_dependencies_page(crate_name, 1)
    }

    /// Get the total count of reverse dependencies for a crate.
    ///
    /// **Note:** Always returns `0`.
    pub fn crate_reverse_dependency_count(&self, _crate_name: &str) -> Result<u64, Error> {
        Ok(0)
    }

    /// Retrieve the authors for a crate version.
    ///
    /// **Note:** Always returns an empty list — see
    /// [`AsyncClient::crate_authors`](crate::AsyncClient::crate_authors).
    pub fn crate_authors(&self, _crate_name: &str, _version: &str) -> Result<Authors, Error> {
        Ok(Authors { names: vec![] })
    }

    /// Retrieve the dependencies for a specific version of a crate.
    ///
    /// Fully supported from the sparse index.
    pub fn crate_dependencies(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Vec<Dependency>, Error> {
        let entries = self.get_index_entries(crate_name)?;
        let entry = entries
            .iter()
            .find(|e| e.vers == version)
            .ok_or_else(|| {
                Error::NotFound(NotFoundError {
                    url: format!("{crate_name}/{version}/dependencies"),
                })
            })?;
        let version_id = synthesize_id(crate_name, version);
        Ok(entry
            .deps
            .iter()
            .map(|d| index_dep_to_dependency(d, version_id))
            .collect())
    }

    /// Build a [`FullVersion`] by fetching its dependency list from the index.
    fn full_version(&self, version: Version) -> Result<FullVersion, Error> {
        let deps = self.crate_dependencies(&version.crate_name, &version.num)?;
        Ok(FullVersion {
            created_at: version.created_at,
            updated_at: version.updated_at,
            dl_path: version.dl_path,
            downloads: version.downloads,
            features: version.features,
            id: version.id,
            num: version.num,
            yanked: version.yanked,
            license: version.license,
            links: version.links,
            readme_path: version.readme_path,
            author_names: vec![],
            dependencies: deps,
        })
    }

    /// Retrieve complete information for a crate.
    ///
    /// Fields not available in the sparse index are set to empty/zero defaults.
    pub fn full_crate(&self, name: &str, all_versions: bool) -> Result<FullCrate, Error> {
        let resp = self.get_crate(name)?;
        let data = resp.crate_data;

        let dls = self.crate_downloads(name)?;
        let owners = self.crate_owners(name)?;
        let reverse_dependencies = self.crate_reverse_dependencies(name)?;

        let versions = if resp.versions.is_empty() {
            vec![]
        } else if all_versions {
            resp.versions
                .into_iter()
                .map(|v| self.full_version(v))
                .collect::<Result<Vec<FullVersion>, Error>>()?
        } else {
            vec![self.full_version(resp.versions[0].clone())?]
        };

        Ok(FullCrate {
            id: data.id,
            name: data.name,
            description: data.description,
            license: versions.first().and_then(|v| v.license.clone()),
            documentation: data.documentation,
            homepage: data.homepage,
            repository: data.repository,
            total_downloads: data.downloads,
            max_version: data.max_version,
            max_stable_version: data.max_stable_version,
            created_at: data.created_at,
            updated_at: data.updated_at,
            categories: resp.categories,
            keywords: resp.keywords,
            downloads: dls,
            owners,
            reverse_dependencies,
            versions,
        })
    }

    /// Retrieve a page of crates matching the given query.
    ///
    /// See [`AsyncClient::crates`](crate::AsyncClient::crates) for
    /// limitations.
    ///
    /// # Examples
    ///
    /// Exact-name lookup:
    ///
    /// ```rust
    /// # use crates_io_api::{SyncClient, CratesQuery, Sort, Error};
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// # let client = SyncClient::new(
    /// #     "my-bot (my-contact@domain.com)",
    /// #     std::time::Duration::from_millis(1000),
    /// # ).unwrap();
    /// let q = CratesQuery::builder().search("serde").build();
    /// let page = client.crates(q)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn crates(&self, query: CratesQuery) -> Result<CratesPage, Error> {
        if let Some(ref search) = query.search {
            if query.page <= 1 {
                if let Ok(resp) = self.get_crate(search) {
                    return Ok(CratesPage {
                        crates: vec![resp.crate_data],
                        versions: resp.versions,
                        keywords: resp.keywords,
                        categories: resp.categories,
                        meta: Meta { total: 1 },
                    });
                }
            }
        }
        Ok(CratesPage {
            crates: vec![],
            versions: vec![],
            keywords: vec![],
            categories: vec![],
            meta: Meta { total: 0 },
        })
    }

    /// Retrieve a user by username.
    ///
    /// **Note:** Always returns [`Error::NotFound`] — user information is not
    /// available in the sparse index.
    pub fn user(&self, username: &str) -> Result<User, Error> {
        Err(Error::NotFound(NotFoundError {
            url: format!("users/{}", username),
        }))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_client() -> SyncClient {
        SyncClient::new(
            "crates-io-api-index-ci (github.com/LuuuXXX/crates-io-api)",
            std::time::Duration::from_millis(1000),
        )
        .unwrap()
    }

    #[test]
    fn test_get_crate() -> Result<(), Error> {
        let client = build_test_client();
        let resp = client.get_crate("serde")?;
        assert_eq!(resp.crate_data.name, "serde");
        assert!(!resp.versions.is_empty());
        assert!(!resp.crate_data.max_version.is_empty());
        Ok(())
    }

    #[test]
    fn test_crate_dependencies() -> Result<(), Error> {
        let client = build_test_client();
        let deps = client.crate_dependencies("serde_json", "1.0.0")?;
        assert!(!deps.is_empty(), "serde_json 1.0.0 should have dependencies");
        assert!(
            deps.iter().any(|d| d.crate_id == "serde"),
            "serde_json should depend on serde"
        );
        Ok(())
    }

    #[test]
    fn test_full_crate() -> Result<(), Error> {
        let client = build_test_client();
        let fc = client.full_crate("log", false)?;
        assert_eq!(fc.name, "log");
        assert!(!fc.versions.is_empty());
        Ok(())
    }

    #[test]
    fn sync_client_ensure_send() {
        let client = build_test_client();
        let _: &dyn Send = &client;
    }

    #[test]
    fn test_get_crate_with_slash() {
        let client = build_test_client();
        match client.get_crate("a/b") {
            Err(Error::NotFound(_)) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_crates_exact_search() -> Result<(), Error> {
        let client = build_test_client();
        let page = client.crates(CratesQuery::builder().search("serde").build())?;
        assert_eq!(page.meta.total, 1);
        assert_eq!(page.crates[0].name, "serde");
        Ok(())
    }

    #[test]
    fn test_summary_empty() -> Result<(), Error> {
        let client = build_test_client();
        let s = client.summary()?;
        assert_eq!(s.num_crates, 0);
        assert!(s.most_downloaded.is_empty());
        Ok(())
    }

    #[test]
    fn test_crate_downloads_empty() -> Result<(), Error> {
        let client = build_test_client();
        let dls = client.crate_downloads("serde")?;
        assert!(dls.version_downloads.is_empty());
        Ok(())
    }

    #[test]
    fn test_crate_owners_empty() -> Result<(), Error> {
        let client = build_test_client();
        let owners = client.crate_owners("serde")?;
        assert!(owners.is_empty());
        Ok(())
    }

    #[test]
    fn test_user_not_found() {
        let client = build_test_client();
        match client.user("theduke") {
            Err(Error::NotFound(_)) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_crate_reverse_dependency_count_zero() -> Result<(), Error> {
        let client = build_test_client();
        let count = client.crate_reverse_dependency_count("serde")?;
        assert_eq!(count, 0);
        Ok(())
    }
}
