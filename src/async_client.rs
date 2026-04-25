//! Asynchronous client backed by the crates.io sparse registry index.

use futures::future::BoxFuture;
use futures::prelude::*;
use log::trace;
use reqwest::{header, Client as HttpClient, StatusCode};
use std::collections::VecDeque;

use crate::{
    error::{Error, NotFoundError, PermissionDeniedError},
    index::{
        build_index_url, empty_crate_downloads, empty_reverse_dependencies, entries_to_crate_response,
        entries_to_dependencies, not_supported, parse_index_entries,
    },
    types::*,
};

/// An asynchronous client for the crates.io sparse registry index.
///
/// Provides the same interface as the original `crates_io_api` async client.
/// See [`SyncClient`](crate::SyncClient) for a table of method availability.
#[derive(Clone)]
pub struct Client {
    client: HttpClient,
    rate_limit: std::time::Duration,
    last_request_time:
        std::sync::Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>,
}

// ── CrateStream (mirrors the base implementation) ────────────────────────────

/// An infinite stream of crates matching a [`CratesQuery`].
///
/// **Note:** Crate enumeration is not available via the sparse registry index.
/// This stream will always yield zero items.
pub struct CrateStream {
    // Fields kept for API compatibility with the base crate.
    #[allow(dead_code)]
    client: Client,
    #[allow(dead_code)]
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

        // The sparse index does not support enumeration: close the stream.
        inner.closed = true;
        std::task::Poll::Ready(None)
    }
}

impl Client {
    /// Instantiate a new async client.
    ///
    /// Returns an error if the given user agent string is not a valid HTTP
    /// header value.
    ///
    /// # Example
    ///
    /// ```rust
    /// # fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = crates_io_api::AsyncClient::new(
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
            .expect("failed to build reqwest async client");

        Ok(Self::with_http_client(client, rate_limit))
    }

    /// Instantiate a new client from a pre-built [`reqwest::Client`].
    pub fn with_http_client(client: HttpClient, rate_limit: std::time::Duration) -> Self {
        Self {
            client,
            rate_limit,
            last_request_time: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    // ── Internal HTTP helper ─────────────────────────────────────────────────

    async fn fetch_text(&self, url: url::Url) -> Result<String, Error> {
        trace!("GET {}", url);

        let mut lock = self.last_request_time.clone().lock_owned().await;
        if let Some(last) = *lock {
            let elapsed = last.elapsed();
            if elapsed < self.rate_limit {
                tokio::time::sleep(self.rate_limit - elapsed).await;
            }
        }

        let request_start = tokio::time::Instant::now();
        let res = self.client.get(url.clone()).send().await?;

        if !res.status().is_success() {
            return Err(match res.status() {
                StatusCode::NOT_FOUND => Error::NotFound(NotFoundError {
                    url: url.to_string(),
                }),
                StatusCode::FORBIDDEN => {
                    let reason = res.text().await.unwrap_or_default();
                    Error::PermissionDenied(PermissionDeniedError { reason })
                }
                _ => Error::from(res.error_for_status().unwrap_err()),
            });
        }

        *lock = Some(request_start);
        Ok(res.text().await?)
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Retrieve information about a crate from the sparse registry index.
    pub async fn get_crate(&self, crate_name: &str) -> Result<CrateResponse, Error> {
        let url = build_index_url(crate_name)?;
        let body = self.fetch_text(url).await?;
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

    /// Retrieve the dependencies of a specific crate version.
    pub async fn crate_dependencies(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Vec<Dependency>, Error> {
        let url = build_index_url(crate_name)?;
        let body = self.fetch_text(url).await?;
        let entries = parse_index_entries(&body);
        entries_to_dependencies(version, &entries).ok_or_else(|| {
            Error::NotFound(NotFoundError {
                url: format!(
                    "crate '{crate_name}' version '{version}' not found in sparse index"
                ),
            })
        })
    }

    /// Retrieve download statistics for a crate.
    ///
    /// **Note:** Always returns empty stats (not available via sparse index).
    pub async fn crate_downloads(&self, _crate_name: &str) -> Result<CrateDownloads, Error> {
        Ok(empty_crate_downloads())
    }

    /// Retrieve the owners of a crate.
    ///
    /// **Note:** Always returns an empty list (not available via sparse index).
    pub async fn crate_owners(&self, _name: &str) -> Result<Vec<User>, Error> {
        Ok(vec![])
    }

    /// Get a single page of reverse dependencies.
    ///
    /// **Note:** Always returns an empty page (not available via sparse index).
    pub async fn crate_reverse_dependencies_page(
        &self,
        _crate_name: &str,
        _page: u64,
    ) -> Result<ReverseDependencies, Error> {
        Ok(empty_reverse_dependencies())
    }

    /// Load all reverse dependencies of a crate.
    ///
    /// **Note:** Always returns an empty result (not available via sparse index).
    pub async fn crate_reverse_dependencies(
        &self,
        _crate_name: &str,
    ) -> Result<ReverseDependencies, Error> {
        Ok(empty_reverse_dependencies())
    }

    /// Get the total count of reverse dependencies.
    ///
    /// **Note:** Always returns `0` (not available via sparse index).
    pub async fn crate_reverse_dependency_count(
        &self,
        _crate_name: &str,
    ) -> Result<u64, Error> {
        Ok(0)
    }

    /// Retrieve the authors for a crate version.
    ///
    /// **Note:** Always returns empty authors (not available via sparse index).
    pub async fn crate_authors(
        &self,
        _crate_name: &str,
        _version: &str,
    ) -> Result<Authors, Error> {
        Ok(Authors { names: vec![] })
    }

    /// Retrieve all available information for a crate.
    pub async fn full_crate(&self, name: &str, all_versions: bool) -> Result<FullCrate, Error> {
        // Single fetch: build CrateResponse and dep entries in one round-trip.
        let url = build_index_url(name)?;
        let body = self.fetch_text(url).await?;
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
            resp.versions.last().into_iter().collect()
        };

        let full_versions: Vec<FullVersion> = versions_to_process
            .iter()
            .map(|v| {
                let deps = entries_to_dependencies(&v.num, &entries).unwrap_or_default();
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

        let license = full_versions.last().and_then(|v| v.license.clone());

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
    /// **Note:** Always returns `Error::Api` (not available via sparse index).
    pub async fn crates(&self, _query: CratesQuery) -> Result<CratesPage, Error> {
        Err(not_supported("crates"))
    }

    /// Get a stream over crates matching the given query.
    ///
    /// **Note:** The stream will always be empty (enumeration not available via
    /// sparse index).
    pub fn crates_stream(&self, filter: CratesQuery) -> CrateStream {
        CrateStream::new(self.clone(), filter)
    }

    /// Retrieve a summary containing crates.io-wide statistics.
    ///
    /// **Note:** Always returns `Error::Api` (not available via sparse index).
    pub async fn summary(&self) -> Result<Summary, Error> {
        Err(not_supported("summary"))
    }

    /// Retrieve a user by username.
    ///
    /// **Note:** Always returns `Error::Api` (not available via sparse index).
    pub async fn user(&self, _username: &str) -> Result<User, Error> {
        Err(not_supported("user"))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn test_client() -> Client {
        Client::new(
            "crates-io-api-test (github.com/LuuuXXX/crates-io-api)",
            std::time::Duration::from_millis(500),
        )
        .unwrap()
    }

    #[test]
    fn async_client_is_send() {
        let client = test_client();
        let _: &dyn Send = &client;
    }

    #[test]
    fn new_rejects_invalid_user_agent() {
        let result = Client::new("bad\nagent", std::time::Duration::ZERO);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn crate_downloads_returns_empty() {
        let client = test_client();
        let dl = client.crate_downloads("serde").await.unwrap();
        assert!(dl.version_downloads.is_empty());
    }

    #[tokio::test]
    async fn crate_owners_returns_empty() {
        let client = test_client();
        let owners = client.crate_owners("serde").await.unwrap();
        assert!(owners.is_empty());
    }

    #[tokio::test]
    async fn summary_returns_error() {
        let client = test_client();
        assert!(matches!(client.summary().await, Err(Error::Api(_))));
    }

    #[tokio::test]
    async fn user_returns_error() {
        let client = test_client();
        assert!(matches!(client.user("ferris").await, Err(Error::Api(_))));
    }

    #[tokio::test]
    async fn crates_returns_error() {
        let client = test_client();
        assert!(matches!(
            client.crates(CratesQuery::default()).await,
            Err(Error::Api(_))
        ));
    }

    #[tokio::test]
    async fn crates_stream_is_empty() {
        let client = test_client();
        let mut stream = client.crates_stream(CratesQuery::default());
        let item = stream.next().await;
        assert!(item.is_none());
    }

    // ── Integration tests (require network) ──────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn integration_get_crate_serde() {
        let client = test_client();
        let resp = client.get_crate("serde").await.unwrap();
        assert_eq!(resp.crate_data.name, "serde");
        assert!(!resp.versions.is_empty());
        semver::Version::parse(&resp.crate_data.max_version).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn integration_get_crate_not_found() {
        let client = test_client();
        let result = client
            .get_crate("this-crate-definitely-does-not-exist-xyz-42")
            .await;
        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[tokio::test]
    #[ignore]
    async fn integration_get_crate_with_slash() {
        let client = test_client();
        let result = client.get_crate("a/b").await;
        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[tokio::test]
    #[ignore]
    async fn integration_crate_dependencies() {
        let client = test_client();
        let deps = client.crate_dependencies("serde", "1.0.0").await.unwrap();
        assert!(deps.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn integration_full_crate() {
        let client = test_client();
        let full = client.full_crate("serde", false).await.unwrap();
        assert_eq!(full.name, "serde");
    }
}
