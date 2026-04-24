//! Asynchronous client backed by the crates.io sparse registry index.

use futures::future::{try_join_all, BoxFuture};
use futures::prelude::*;
use reqwest::{header, Client as HttpClient, StatusCode, Url};
use std::collections::VecDeque;

use super::Error;
use crate::convert::{
    entries_to_crate, entry_to_version, index_dep_to_dependency, synthesize_id,
};
use crate::error::{JsonDecodeError, NotFoundError, PermissionDeniedError};
use crate::index::{index_path, parse_index_file, IndexEntry};
use crate::types::*;

/// Base URL of the crates.io sparse registry index.
const INDEX_BASE: &str = "https://index.crates.io/";

// ---------------------------------------------------------------------------
// CrateStream
// ---------------------------------------------------------------------------

/// A stream of [`Crate`] items from a paginated query.
///
/// Created by [`Client::crates_stream`].  Because the sparse index does not
/// support arbitrary listing, the stream terminates after the first page that
/// returns no crates.
pub struct CrateStream {
    client: Client,
    filter: CratesQuery,
    closed: bool,
    items: VecDeque<Crate>,
    next_page_fetch: Option<BoxFuture<'static, Result<CratesPage, Error>>>,
}

impl CrateStream {
    fn new(client: Client, filter: CratesQuery) -> Self {
        Self {
            client,
            filter,
            closed: false,
            items: VecDeque::new(),
            next_page_fetch: None,
        }
    }
}

impl futures::stream::Stream for CrateStream {
    type Item = Result<Crate, Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let inner = self.get_mut();

        if inner.closed {
            return std::task::Poll::Ready(None);
        }

        if let Some(krate) = inner.items.pop_front() {
            return std::task::Poll::Ready(Some(Ok(krate)));
        }

        if let Some(mut fut) = inner.next_page_fetch.take() {
            return match fut.poll_unpin(cx) {
                std::task::Poll::Ready(res) => match res {
                    Ok(page) if page.crates.is_empty() => {
                        inner.closed = true;
                        std::task::Poll::Ready(None)
                    }
                    Ok(page) => {
                        let mut iter = page.crates.into_iter();
                        let next = iter.next();
                        inner.items.extend(iter);
                        std::task::Poll::Ready(next.map(Ok))
                    }
                    Err(err) => {
                        inner.closed = true;
                        std::task::Poll::Ready(Some(Err(err)))
                    }
                },
                std::task::Poll::Pending => {
                    inner.next_page_fetch = Some(fut);
                    std::task::Poll::Pending
                }
            };
        }

        let filter = inner.filter.clone();
        inner.filter.page += 1;

        let c = inner.client.clone();
        let mut f = Box::pin(async move { c.crates(filter).await });
        assert!(matches!(f.poll_unpin(cx), std::task::Poll::Pending));
        inner.next_page_fetch = Some(f);

        cx.waker().clone().wake();
        std::task::Poll::Pending
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Asynchronous client for the crates.io **sparse registry index**.
///
/// Instead of querying the crates.io REST web API, this client fetches data
/// directly from the official sparse registry index at
/// `https://index.crates.io/`.  This is the same data source that Cargo uses
/// for dependency resolution.
///
/// # What is available
///
/// | Capability                         | Status                                    |
/// |------------------------------------|-------------------------------------------|
/// | Version list, features, `yanked`   | ✓ Full                                    |
/// | Dependency tree (per version)      | ✓ Full                                    |
/// | Max / max-stable version           | ✓ Derived from index                      |
/// | Download counts                    | ✗ Always 0                               |
/// | Owners                             | ✗ Always empty                           |
/// | Authors                            | ✗ Always empty (not in index)            |
/// | Reverse dependencies               | ✗ Always empty (requires full scan)      |
/// | Summary statistics                 | ✗ Always zero / empty                    |
/// | Crate search / listing             | ✓ Exact-name lookup only                 |
/// | User lookup                        | ✗ Not available                          |
///
/// # Rate limiting
///
/// At most one request will be issued within the configured `rate_limit`
/// duration.  The crates.io [Crawler Policy] recommends ≥ 1 second between
/// requests.
///
/// [Crawler Policy]: https://crates.io/policies#crawlers
#[derive(Clone)]
pub struct Client {
    client: HttpClient,
    rate_limit: std::time::Duration,
    last_request_time: std::sync::Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>,
    base_url: Url,
}

impl Client {
    /// Create a new client.
    ///
    /// Returns an error if `user_agent` contains invalid header characters.
    ///
    /// ```rust
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = crates_io_api::AsyncClient::new(
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
        let client = HttpClient::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Ok(Self::with_http_client(client, rate_limit))
    }

    /// Create a client from an already-configured [`reqwest::Client`].
    pub fn with_http_client(client: HttpClient, rate_limit: std::time::Duration) -> Self {
        Self {
            rate_limit,
            last_request_time: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            client,
            base_url: Url::parse(INDEX_BASE).expect("static base URL is valid"),
        }
    }

    // -----------------------------------------------------------------------
    // Internal HTTP helper
    // -----------------------------------------------------------------------

    /// Perform a rate-limited GET request and return the response body as text.
    async fn get_text(&self, url: &Url) -> Result<String, Error> {
        let mut lock = self.last_request_time.clone().lock_owned().await;

        if let Some(last) = lock.take() {
            let elapsed = last.elapsed();
            if elapsed < self.rate_limit {
                tokio::time::sleep(self.rate_limit - elapsed).await;
            }
        }

        let time = tokio::time::Instant::now();
        let res = self.client.get(url.clone()).send().await?;

        if !res.status().is_success() {
            let err = match res.status() {
                StatusCode::NOT_FOUND => Error::NotFound(NotFoundError {
                    url: url.to_string(),
                }),
                StatusCode::FORBIDDEN => {
                    let reason = res.text().await.unwrap_or_default();
                    Error::PermissionDenied(PermissionDeniedError { reason })
                }
                _ => Error::from(res.error_for_status().unwrap_err()),
            };
            return Err(err);
        }

        *lock = Some(time);
        res.text().await.map_err(Error::from)
    }

    // -----------------------------------------------------------------------
    // Index fetch
    // -----------------------------------------------------------------------

    /// Fetch and parse all version entries for `crate_name` from the index.
    ///
    /// Returns [`Error::NotFound`] when the crate is absent from the index.
    async fn get_index_entries(&self, crate_name: &str) -> Result<Vec<IndexEntry>, Error> {
        // Guard against embedded slashes (same behaviour as web API client).
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
        let content = self.get_text(&url).await?;
        parse_index_file(&content).map_err(|e| {
            Error::JsonDecode(JsonDecodeError {
                message: format!("Failed to parse index entry for '{}': {}", crate_name, e),
            })
        })
    }

    // -----------------------------------------------------------------------
    // Public API (same signatures as base crates_io_api::AsyncClient)
    // -----------------------------------------------------------------------

    /// Retrieve a summary of crates.io statistics.
    ///
    /// **Note:** The sparse index does not provide global statistics.  This
    /// method always returns an empty [`Summary`] (zero counts, empty lists).
    /// If summary statistics are required, use the crates.io web API client
    /// from the `base` crate instead.
    pub async fn summary(&self) -> Result<Summary, Error> {
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
    ///
    /// The returned [`CrateResponse`] is fully populated for fields available
    /// in the sparse index.  Fields such as `description`, `homepage`,
    /// `downloads`, `categories`, and `keywords` are not present in the index
    /// and are set to `None` / `0` / empty.
    pub async fn get_crate(&self, crate_name: &str) -> Result<CrateResponse, Error> {
        let entries = self.get_index_entries(crate_name).await?;
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
    /// **Note:** The sparse index does not contain download data.  This method
    /// always returns an empty [`CrateDownloads`].
    pub async fn crate_downloads(&self, _crate_name: &str) -> Result<CrateDownloads, Error> {
        Ok(CrateDownloads {
            version_downloads: vec![],
            meta: CrateDownloadsMeta {
                extra_downloads: vec![],
            },
        })
    }

    /// Retrieve the owners of a crate.
    ///
    /// **Note:** Ownership information is not available in the sparse index.
    /// This method always returns an empty list.
    pub async fn crate_owners(&self, _crate_name: &str) -> Result<Vec<User>, Error> {
        Ok(vec![])
    }

    /// Retrieve a single page of reverse dependencies.
    ///
    /// **Note:** Reverse dependencies require scanning the full index, which
    /// is not supported in real time.  This method always returns an empty
    /// [`ReverseDependencies`].
    pub async fn crate_reverse_dependencies_page(
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
    /// **Note:** Always returns an empty [`ReverseDependencies`] — see
    /// [`crate_reverse_dependencies_page`](Self::crate_reverse_dependencies_page).
    pub async fn crate_reverse_dependencies(
        &self,
        crate_name: &str,
    ) -> Result<ReverseDependencies, Error> {
        self.crate_reverse_dependencies_page(crate_name, 1).await
    }

    /// Get the total count of reverse dependencies for a crate.
    ///
    /// **Note:** Always returns `0` — see
    /// [`crate_reverse_dependencies_page`](Self::crate_reverse_dependencies_page).
    pub async fn crate_reverse_dependency_count(
        &self,
        _crate_name: &str,
    ) -> Result<u64, Error> {
        Ok(0)
    }

    /// Retrieve the authors for a crate version.
    ///
    /// **Note:** Author information is not present in the sparse index.  This
    /// method always returns an empty [`Authors`] list.
    pub async fn crate_authors(
        &self,
        _crate_name: &str,
        _version: &str,
    ) -> Result<Authors, Error> {
        Ok(Authors { names: vec![] })
    }

    /// Retrieve the dependencies for a specific version of a crate.
    ///
    /// This is fully supported: the sparse index stores the complete dependency
    /// list for every published version.
    pub async fn crate_dependencies(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Vec<Dependency>, Error> {
        let entries = self.get_index_entries(crate_name).await?;
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
    async fn full_version(&self, version: Version) -> Result<FullVersion, Error> {
        let deps = self
            .crate_dependencies(&version.crate_name, &version.num)
            .await?;
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
            // Authors not available in the index.
            author_names: vec![],
            dependencies: deps,
        })
    }

    /// Retrieve complete information for a crate.
    ///
    /// The `all_versions` flag controls whether detailed information is
    /// fetched for every version or only for the most recent one.  Each
    /// version requires one additional index request.
    ///
    /// Fields not available in the sparse index (downloads, owners, reverse
    /// dependencies, authors, license, …) are set to empty/zero defaults.
    pub async fn full_crate(&self, name: &str, all_versions: bool) -> Result<FullCrate, Error> {
        let krate = self.get_crate(name).await?;
        let versions = if krate.versions.is_empty() {
            vec![]
        } else if all_versions {
            try_join_all(
                krate
                    .versions
                    .clone()
                    .into_iter()
                    .map(|v| self.full_version(v)),
            )
            .await?
        } else {
            vec![self.full_version(krate.versions[0].clone()).await?]
        };

        let dls = self.crate_downloads(name).await?;
        let owners = self.crate_owners(name).await?;
        let reverse_dependencies = self.crate_reverse_dependencies(name).await?;
        let data = krate.crate_data;

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
            categories: krate.categories,
            keywords: krate.keywords,
            downloads: dls,
            owners,
            reverse_dependencies,
            versions,
        })
    }

    /// Retrieve a page of crates matching the given query.
    ///
    /// **Index limitations:**
    /// - If `query.search` is set, an exact-name lookup is attempted against
    ///   the index.  Fuzzy / prefix search is not supported.
    /// - If `query.user_id` or `query.category` filters are set without a
    ///   search term, an empty page is returned.
    /// - Pagination beyond page 1 always returns an empty page (the index does
    ///   not support listing).
    pub async fn crates(&self, query: CratesQuery) -> Result<CratesPage, Error> {
        // Exact-name lookup when a search term is supplied.
        if let Some(ref search) = query.search {
            if query.page <= 1 {
                if let Ok(resp) = self.get_crate(search).await {
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

    /// Get a stream over all crates matching `filter`.
    ///
    /// The stream respects the same limitations as [`crates`](Self::crates).
    pub fn crates_stream(&self, filter: CratesQuery) -> CrateStream {
        CrateStream::new(self.clone(), filter)
    }

    /// Retrieve a user by username.
    ///
    /// **Note:** User information is not available in the sparse index.  This
    /// method always returns [`Error::NotFound`].
    pub async fn user(&self, username: &str) -> Result<User, Error> {
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

    fn build_test_client() -> Client {
        Client::new(
            "crates-io-api-index-ci (github.com/LuuuXXX/crates-io-api)",
            std::time::Duration::from_millis(1000),
        )
        .unwrap()
    }

    /// Verify that `get_crate` returns a populated `CrateResponse` for a
    /// well-known crate.
    #[tokio::test]
    async fn test_get_crate_async() -> Result<(), Error> {
        let client = build_test_client();
        let resp = client.get_crate("serde").await?;
        assert_eq!(resp.crate_data.name, "serde");
        assert!(!resp.versions.is_empty(), "serde should have many versions");
        assert!(
            !resp.crate_data.max_version.is_empty(),
            "max_version should be set"
        );
        Ok(())
    }

    /// Verify that the dependency list for a specific version is populated.
    #[tokio::test]
    async fn test_crate_dependencies_async() -> Result<(), Error> {
        let client = build_test_client();
        // serde 1.0.0 has no dependencies.
        let deps = client.crate_dependencies("serde", "1.0.0").await?;
        // Check that the call succeeds and returns a Vec (may be empty).
        let _ = deps;
        Ok(())
    }

    /// Verify that the dependency list for a version with known deps works.
    #[tokio::test]
    async fn test_crate_dependencies_nonempty_async() -> Result<(), Error> {
        let client = build_test_client();
        // serde_json depends on serde.
        let deps = client.crate_dependencies("serde_json", "1.0.0").await?;
        assert!(
            !deps.is_empty(),
            "serde_json 1.0.0 should have dependencies"
        );
        assert!(
            deps.iter().any(|d| d.crate_id == "serde"),
            "serde_json should depend on serde"
        );
        Ok(())
    }

    /// Verify the full_crate helper returns a valid FullCrate.
    #[tokio::test]
    async fn test_full_crate_async() -> Result<(), Error> {
        let client = build_test_client();
        let fc = client.full_crate("log", false).await?;
        assert_eq!(fc.name, "log");
        assert!(!fc.versions.is_empty());
        Ok(())
    }

    /// Verify that looking up a crate with a slash returns NotFound.
    #[tokio::test]
    async fn test_get_crate_with_slash_async() {
        let client = build_test_client();
        match client.get_crate("a/b").await {
            Err(Error::NotFound(_)) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    /// Verify that the exact-name search path of `crates()` works.
    #[tokio::test]
    async fn test_crates_exact_search_async() -> Result<(), Error> {
        let client = build_test_client();
        let page = client
            .crates(CratesQuery::builder().search("log").build())
            .await?;
        assert_eq!(page.meta.total, 1);
        assert_eq!(page.crates[0].name, "log");
        Ok(())
    }

    /// Verify that `summary()` returns without error (data will be empty).
    #[tokio::test]
    async fn test_summary_async() -> Result<(), Error> {
        let client = build_test_client();
        let s = client.summary().await?;
        // Stats are not available from the index.
        assert_eq!(s.num_crates, 0);
        assert!(s.most_downloaded.is_empty());
        Ok(())
    }

    /// Verify that `crate_downloads` returns an empty result without error.
    #[tokio::test]
    async fn test_crate_downloads_async() -> Result<(), Error> {
        let client = build_test_client();
        let dls = client.crate_downloads("serde").await?;
        assert!(dls.version_downloads.is_empty());
        Ok(())
    }

    /// Verify that `crate_owners` returns an empty list without error.
    #[tokio::test]
    async fn test_crate_owners_async() -> Result<(), Error> {
        let client = build_test_client();
        let owners = client.crate_owners("serde").await?;
        assert!(owners.is_empty());
        Ok(())
    }

    /// Verify that `user()` returns NotFound.
    #[tokio::test]
    async fn test_user_not_found_async() {
        let client = build_test_client();
        match client.user("theduke").await {
            Err(Error::NotFound(_)) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }
}
