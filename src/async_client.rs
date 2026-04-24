//! Asynchronous client backed by the crates.io sparse registry index.
//!
//! The primary type is [`Client`] (re-exported as `AsyncClient`).
//! Use [`Client::new`] to create a client and then call the methods
//! on it to query the registry.
//!
//! # Example
//!
//! ```rust,no_run
//! use crates_io_api::{AsyncClient, Error};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Error> {
//!     let client = AsyncClient::new(
//!         "my-bot (contact@example.com)",
//!         std::time::Duration::from_millis(1000),
//!     )?;
//!     let krate = client.get_crate("serde").await?;
//!     println!("serde max version: {}", krate.crate_data.max_version);
//!     Ok(())
//! }
//! ```

use futures::future::{try_join_all, BoxFuture};
use futures::prelude::*;
use reqwest::{header, Client as HttpClient, StatusCode, Url};
use serde::de::DeserializeOwned;
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
/// Base URL of the crates.io REST API (used as fallback for methods not
/// available in the sparse index).
const API_BASE: &str = "https://crates.io/api/v1/";

// ---------------------------------------------------------------------------
// CrateStream
// ---------------------------------------------------------------------------

/// A stream of [`Crate`] items from a paginated query.
///
/// Created by [`Client::crates_stream`].  Because the sparse index does not
/// support arbitrary listing, the stream terminates after the first page that
/// returns no crates.
///
/// The stream implements [`futures::stream::Stream`] and can be used with the
/// standard combinators in [`futures::stream::StreamExt`].
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

        // Yield any buffered items from the previous page first.
        if let Some(krate) = inner.items.pop_front() {
            return std::task::Poll::Ready(Some(Ok(krate)));
        }

        // Get an in-flight future (existing) or create one for the next page.
        let mut f = if let Some(fut) = inner.next_page_fetch.take() {
            fut
        } else {
            let filter = inner.filter.clone();
            inner.filter.page += 1;
            let c = inner.client.clone();
            Box::pin(async move { c.crates(filter).await })
        };

        // Poll the future and handle both Ready and Pending.  `crates()` can
        // resolve synchronously (e.g., when no search term returns an empty
        // page immediately), so we must not assume it is always Pending.
        match f.poll_unpin(cx) {
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
                inner.next_page_fetch = Some(f);
                std::task::Poll::Pending
            }
        }
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
/// for dependency resolution.  For data not available in the sparse index
/// (download stats, owners, authors, reverse dependencies, summary
/// statistics, user lookup), the client automatically falls back to the
/// crates.io REST API at `https://crates.io/api/v1/`.
///
/// # What is available
///
/// | Capability                         | Status                                    |
/// |------------------------------------|-------------------------------------------|
/// | Version list, features, `yanked`   | ✓ Full (index)                            |
/// | Dependency tree (per version)      | ✓ Full (index)                            |
/// | Max / max-stable version           | ✓ Derived from index                      |
/// | Download counts                    | ✓ REST API fallback                       |
/// | Owners                             | ✓ REST API fallback                       |
/// | Authors                            | ✓ REST API fallback                       |
/// | Reverse dependencies               | ✓ REST API fallback                       |
/// | Summary statistics                 | ✓ REST API fallback                       |
/// | Crate search / listing             | ✓ Exact-name lookup only                 |
/// | User lookup                        | ✓ REST API fallback                       |
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
    api_base_url: Url,
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
            .expect("reqwest async client build should not fail; check TLS/proxy configuration");
        Ok(Self::with_http_client(client, rate_limit))
    }

    /// Create a client from an already-configured [`reqwest::Client`].
    ///
    /// Use this when you need custom TLS configuration, a proxy, or other
    /// advanced `reqwest` settings.
    pub fn with_http_client(client: HttpClient, rate_limit: std::time::Duration) -> Self {
        Self {
            rate_limit,
            last_request_time: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            client,
            base_url: Url::parse(INDEX_BASE).expect("static base URL is valid"),
            api_base_url: Url::parse(API_BASE).expect("static API base URL is valid"),
        }
    }

    // -----------------------------------------------------------------------
    // Internal HTTP helper
    // -----------------------------------------------------------------------

    /// Perform a rate-limited GET request and return the response body as text.
    ///
    /// The mutex is held only long enough to compute the required sleep
    /// duration and record the current instant.  The lock is released before
    /// sleeping and before the network request, so concurrent callers can
    /// schedule their own delays without serialising on I/O.
    async fn get_text(&self, url: &Url) -> Result<String, Error> {
        let delay = {
            let mut lock = self.last_request_time.clone().lock_owned().await;
            let now = tokio::time::Instant::now();
            let delay = lock.map(|last| {
                let elapsed = now.duration_since(last);
                if elapsed < self.rate_limit {
                    self.rate_limit - elapsed
                } else {
                    std::time::Duration::ZERO
                }
            });
            // Record the time now so concurrent callers see an up-to-date
            // value while we sleep / wait for the network.
            *lock = Some(now);
            delay
        }; // lock released here — no I/O happens while it is held

        if let Some(d) = delay {
            if !d.is_zero() {
                tokio::time::sleep(d).await;
            }
        }

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

        res.text().await.map_err(Error::from)
    }

    /// Perform a rate-limited GET request to the REST API and deserialize the
    /// JSON response body into `T`.
    async fn get_json<T: DeserializeOwned>(&self, url: &Url) -> Result<T, Error> {
        let content = self.get_text(url).await?;

        if let Ok(errors) = serde_json::from_str::<ApiErrors>(&content) {
            return Err(Error::Api(errors));
        }

        serde_json::from_str::<T>(&content).map_err(|e| {
            Error::JsonDecode(JsonDecodeError {
                message: format!("Could not decode JSON from {url}: {e}"),
            })
        })
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
    /// Falls back to the crates.io REST API (`/api/v1/summary`).
    pub async fn summary(&self) -> Result<Summary, Error> {
        let url = self.api_base_url.join("summary").map_err(Error::from)?;
        self.get_json(&url).await
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
    /// Falls back to the crates.io REST API (`/api/v1/crates/{name}/downloads`).
    pub async fn crate_downloads(&self, crate_name: &str) -> Result<CrateDownloads, Error> {
        let url = build_api_crate_url(&self.api_base_url, crate_name)?
            .join("downloads")
            .map_err(Error::from)?;
        self.get_json(&url).await
    }

    /// Retrieve the owners of a crate.
    ///
    /// Falls back to the crates.io REST API (`/api/v1/crates/{name}/owners`).
    pub async fn crate_owners(&self, crate_name: &str) -> Result<Vec<User>, Error> {
        let url = build_api_crate_url(&self.api_base_url, crate_name)?
            .join("owners")
            .map_err(Error::from)?;
        self.get_json::<Owners>(&url).await.map(|o| o.users)
    }

    /// Retrieve a single page of reverse dependencies.
    ///
    /// Falls back to the crates.io REST API
    /// (`/api/v1/crates/{name}/reverse_dependencies`).
    pub async fn crate_reverse_dependencies_page(
        &self,
        crate_name: &str,
        page: u64,
    ) -> Result<ReverseDependencies, Error> {
        let page = page.max(1);
        let url = build_api_crate_url(&self.api_base_url, crate_name)?
            .join(&format!("reverse_dependencies?per_page=100&page={page}"))
            .map_err(Error::from)?;
        let raw = self.get_json::<ReverseDependenciesAsReceived>(&url).await?;
        let mut deps = ReverseDependencies {
            dependencies: vec![],
            meta: Meta { total: 0 },
        };
        deps.extend(raw);
        Ok(deps)
    }

    /// Retrieve all reverse dependencies of a crate.
    ///
    /// Falls back to the crates.io REST API, paginating automatically.
    pub async fn crate_reverse_dependencies(
        &self,
        crate_name: &str,
    ) -> Result<ReverseDependencies, Error> {
        let mut all = ReverseDependencies {
            dependencies: vec![],
            meta: Meta { total: 0 },
        };
        for page_number in 1.. {
            let page = self
                .crate_reverse_dependencies_page(crate_name, page_number)
                .await?;
            if page.dependencies.is_empty() {
                break;
            }
            all.dependencies.extend(page.dependencies);
            all.meta.total = page.meta.total;
        }
        Ok(all)
    }

    /// Get the total count of reverse dependencies for a crate.
    ///
    /// Falls back to the crates.io REST API.
    pub async fn crate_reverse_dependency_count(
        &self,
        crate_name: &str,
    ) -> Result<u64, Error> {
        let page = self.crate_reverse_dependencies_page(crate_name, 1).await?;
        Ok(page.meta.total)
    }

    /// Retrieve the authors for a crate version.
    ///
    /// Falls back to the crates.io REST API
    /// (`/api/v1/crates/{name}/{version}/authors`).
    pub async fn crate_authors(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Authors, Error> {
        let url = build_api_crate_url(&self.api_base_url, crate_name)?
            .join(&format!("{version}/authors"))
            .map_err(Error::from)?;
        self.get_json::<AuthorsResponse>(&url)
            .await
            .map(|r| Authors { names: r.meta.names })
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

    /// Build a [`FullVersion`] by fetching its dependency list from the index
    /// and its author list from the REST API.
    async fn full_version(&self, version: Version) -> Result<FullVersion, Error> {
        let (authors, deps) = futures::try_join!(
            self.crate_authors(&version.crate_name, &version.num),
            self.crate_dependencies(&version.crate_name, &version.num),
        )?;
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
            author_names: authors.names,
            dependencies: deps,
        })
    }

    /// Retrieve complete information for a crate.
    ///
    /// The `all_versions` flag controls whether detailed information is
    /// fetched for every version or only for the most recent one.  Version
    /// data (dependencies, authors) is fetched from the sparse index and the
    /// REST API respectively.
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
    /// Falls back to the crates.io REST API (`/api/v1/users/{username}`).
    pub async fn user(&self, username: &str) -> Result<User, Error> {
        let url = self
            .api_base_url
            .join(&format!("users/{}", username))
            .map_err(Error::from)?;
        self.get_json::<UserResponse>(&url).await.map(|r| r.user)
    }
}

// ---------------------------------------------------------------------------
// REST API URL helpers
// ---------------------------------------------------------------------------

/// Build a URL for the REST API's `/crates/{name}/` path segment.
///
/// Returns `Err(NotFound)` when `crate_name` contains a slash.
fn build_api_crate_url(base: &Url, crate_name: &str) -> Result<Url, Error> {
    if crate_name.contains('/') {
        return Err(Error::NotFound(NotFoundError {
            url: format!("{base}crates/{crate_name}"),
        }));
    }
    let mut url = base.join("crates/").map_err(Error::from)?;
    url.path_segments_mut()
        .expect("API base URL always has a base")
        .push(crate_name)
        .push("");
    Ok(url)
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
    #[ignore = "requires network access to crates.io"]
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
    #[ignore = "requires network access to crates.io"]
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
    #[ignore = "requires network access to crates.io"]
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
    #[ignore = "requires network access to crates.io"]
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
    #[ignore = "requires network access to crates.io"]
    async fn test_crates_exact_search_async() -> Result<(), Error> {
        let client = build_test_client();
        let page = client
            .crates(CratesQuery::builder().search("log").build())
            .await?;
        assert_eq!(page.meta.total, 1);
        assert_eq!(page.crates[0].name, "log");
        Ok(())
    }

    /// Verify that `summary()` returns real data from the REST API fallback.
    #[tokio::test]
    #[ignore = "requires network access to crates.io"]
    async fn test_summary_async() -> Result<(), Error> {
        let client = build_test_client();
        let s = client.summary().await?;
        assert!(s.num_crates > 0, "num_crates should be non-zero");
        assert!(!s.most_downloaded.is_empty(), "most_downloaded should be non-empty");
        Ok(())
    }

    /// Verify that `crate_downloads` returns real data from the REST API fallback.
    #[tokio::test]
    #[ignore = "requires network access to crates.io"]
    async fn test_crate_downloads_async() -> Result<(), Error> {
        let client = build_test_client();
        let dls = client.crate_downloads("serde").await?;
        assert!(
            !dls.version_downloads.is_empty(),
            "serde should have download data"
        );
        Ok(())
    }

    /// Verify that `crate_owners` returns real data from the REST API fallback.
    #[tokio::test]
    #[ignore = "requires network access to crates.io"]
    async fn test_crate_owners_async() -> Result<(), Error> {
        let client = build_test_client();
        let owners = client.crate_owners("serde").await?;
        assert!(!owners.is_empty(), "serde should have owners");
        Ok(())
    }

    /// Verify that `user()` returns a real user via the REST API fallback.
    #[tokio::test]
    #[ignore = "requires network access to crates.io"]
    async fn test_user_async() -> Result<(), Error> {
        let client = build_test_client();
        let user = client.user("theduke").await?;
        assert_eq!(user.login, "theduke");
        Ok(())
    }

    /// Verify that `crate_reverse_dependency_count` returns a positive number.
    #[tokio::test]
    #[ignore = "requires network access to crates.io"]
    async fn test_crate_reverse_dependency_count_async() -> Result<(), Error> {
        let client = build_test_client();
        let count = client.crate_reverse_dependency_count("serde").await?;
        assert!(count > 0, "serde should have reverse dependencies");
        Ok(())
    }
}
