//! Types for the data available from the crates.io registry.
//!
//! All types are identical to those exposed by the crates.io web API library,
//! so this crate can be used as a **drop-in replacement**.
//!
//! # Data availability
//!
//! Fields that are not present in the sparse registry index (such as
//! `downloads`, `license`, and timestamps on [`Version`]) are either fetched
//! via the REST API fallback or set to a safe default (`0`, `None`, or the
//! Unix epoch).  See the crate-level documentation for the full capability
//! table.

use chrono::{DateTime, NaiveDate, Utc};
use serde_derive::*;
use std::collections::HashMap;

/// API error list returned by the crates.io web API.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiErrors {
    /// Individual errors.
    pub errors: Vec<ApiError>,
}

/// A single API error message.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// Error message detail.
    pub detail: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.detail.as_deref().unwrap_or("Unknown API Error")
        )
    }
}

/// Used to specify the sort behaviour of the `Client::crates()` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sort {
    /// Sort alphabetically.
    Alphabetical,
    /// Sort by relevance (meaningless if used without a query).
    Relevance,
    /// Sort by downloads.
    Downloads,
    /// Sort by recent downloads.
    RecentDownloads,
    /// Sort by recent updates.
    RecentUpdates,
    /// Sort by new.
    NewlyAdded,
}

/// Options for the `crates` method of the client.
///
/// Used to specify pagination, sorting and a query.
#[derive(Clone, Debug)]
pub struct CratesQuery {
    /// Sort order.
    pub(crate) sort: Sort,
    /// Number of items per page.
    pub(crate) per_page: u64,
    /// The page to fetch.
    pub(crate) page: u64,
    /// Filter by owner user id.
    pub(crate) user_id: Option<u64>,
    /// Crates.io category name.
    pub(crate) category: Option<String>,
    /// Search query string.
    pub(crate) search: Option<String>,
}

impl CratesQuery {
    /// Construct a new [`CratesQueryBuilder`].
    pub fn builder() -> CratesQueryBuilder {
        CratesQueryBuilder::new()
    }

    /// Get a reference to the sort.
    pub fn sort(&self) -> &Sort {
        &self.sort
    }

    /// Set the sort.
    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
    }

    /// Get the page size.
    pub fn page_size(&self) -> u64 {
        self.per_page
    }

    /// Set the page size.
    pub fn set_page_size(&mut self, per_page: u64) {
        self.per_page = per_page;
    }

    /// Get the page.
    pub fn page(&self) -> u64 {
        self.page
    }

    /// Set the page.
    pub fn set_page(&mut self, page: u64) {
        self.page = page;
    }

    /// Get the user id filter.
    pub fn user_id(&self) -> Option<u64> {
        self.user_id
    }

    /// Set the user id filter.
    pub fn set_user_id(&mut self, user_id: Option<u64>) {
        self.user_id = user_id;
    }

    /// Get the category filter.
    pub fn category(&self) -> Option<&String> {
        self.category.as_ref()
    }

    /// Set the category filter.
    pub fn set_category(&mut self, category: Option<String>) {
        self.category = category;
    }

    /// Get the search query.
    pub fn search(&self) -> Option<&String> {
        self.search.as_ref()
    }

    /// Set the search query.
    pub fn set_search(&mut self, search: Option<String>) {
        self.search = search;
    }
}

impl Default for CratesQuery {
    fn default() -> Self {
        Self {
            sort: Sort::RecentUpdates,
            per_page: 30,
            page: 1,
            user_id: None,
            category: None,
            search: None,
        }
    }
}

/// Builder for [`CratesQuery`].
///
/// Construct one via [`CratesQuery::builder`], configure it with the
/// chainable setters, and call [`build`](Self::build) to obtain the final
/// [`CratesQuery`].
///
/// # Example
///
/// ```rust
/// use crates_io_api::{CratesQuery, Sort};
///
/// let query = CratesQuery::builder()
///     .search("serde")
///     .sort(Sort::Downloads)
///     .page_size(25)
///     .build();
/// ```
pub struct CratesQueryBuilder {
    query: CratesQuery,
}

impl CratesQueryBuilder {
    /// Construct a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            query: CratesQuery::default(),
        }
    }

    /// Set the sorting method.
    #[must_use]
    pub fn sort(mut self, sort: Sort) -> Self {
        self.query.sort = sort;
        self
    }

    /// Set the page size.
    #[must_use]
    pub fn page_size(mut self, size: u64) -> Self {
        self.query.per_page = size;
        self
    }

    /// Filter by a user id.
    #[must_use]
    pub fn user_id(mut self, user_id: u64) -> Self {
        self.query.user_id = Some(user_id);
        self
    }

    /// Filter by a crates.io category name.
    #[must_use]
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.query.category = Some(category.into());
        self
    }

    /// Filter by a search term.
    #[must_use]
    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.query.search = Some(search.into());
        self
    }

    /// Finalise the builder.
    #[must_use]
    pub fn build(self) -> CratesQuery {
        self.query
    }
}

impl Default for CratesQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Pagination metadata.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Meta {
    /// Total number of results available.
    pub total: u64,
}

/// Links to individual API endpoints for a crate.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct CrateLinks {
    pub owner_team: String,
    pub owner_user: String,
    pub owners: String,
    pub reverse_dependencies: String,
    pub version_downloads: String,
    pub versions: Option<String>,
}

/// A Rust crate in the registry.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Crate {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    // FIXME: Remove on next breaking version bump.
    #[deprecated(
        since = "0.8.1",
        note = "This field is always empty. The license is only available on a specific `Version` of a crate or on `FullCrate`. This field will be removed in the next minor version bump."
    )]
    pub license: Option<String>,
    pub documentation: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub downloads: u64,
    pub recent_downloads: Option<u64>,
    /// NOTE: not set if the crate was loaded via a list query.
    pub categories: Option<Vec<String>>,
    /// NOTE: not set if the crate was loaded via a list query.
    pub keywords: Option<Vec<String>>,
    pub versions: Option<Vec<u64>>,
    pub max_version: String,
    pub max_stable_version: Option<String>,
    pub links: CrateLinks,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub exact_match: Option<bool>,
}

/// A page of crates returned by a list query.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct CratesPage {
    pub crates: Vec<Crate>,
    #[serde(default)]
    pub versions: Vec<Version>,
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    #[serde(default)]
    pub categories: Vec<Category>,
    pub meta: Meta,
}

/// Links to extra data for a crate version.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct VersionLinks {
    #[deprecated(
        since = "0.7.1",
        note = "This field was removed from the API and will always be empty. Will be removed in 0.8.0."
    )]
    #[serde(default)]
    pub authors: String,
    pub dependencies: String,
    pub version_downloads: String,
}

/// A [`Crate`] version.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Version {
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dl_path: String,
    pub downloads: u64,
    pub features: HashMap<String, Vec<String>>,
    pub id: u64,
    pub num: String,
    pub yanked: bool,
    pub license: Option<String>,
    pub readme_path: Option<String>,
    pub links: VersionLinks,
    pub crate_size: Option<u64>,
    pub published_by: Option<User>,
}

/// A crate category.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Category {
    pub category: String,
    pub crates_cnt: u64,
    pub created_at: DateTime<Utc>,
    pub description: String,
    pub id: String,
    pub slug: String,
}

/// A keyword available on crates.io.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Keyword {
    pub id: String,
    pub keyword: String,
    pub crates_cnt: u64,
    pub created_at: DateTime<Utc>,
}

/// Full data for a crate (single-crate lookup response).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct CrateResponse {
    pub categories: Vec<Category>,
    #[serde(rename = "crate")]
    pub crate_data: Crate,
    pub keywords: Vec<Keyword>,
    pub versions: Vec<Version>,
}

/// Summary for crates.io.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Summary {
    pub just_updated: Vec<Crate>,
    pub most_downloaded: Vec<Crate>,
    pub new_crates: Vec<Crate>,
    pub most_recently_downloaded: Vec<Crate>,
    pub num_crates: u64,
    pub num_downloads: u64,
    pub popular_categories: Vec<Category>,
    pub popular_keywords: Vec<Keyword>,
}

/// Download data for a single crate version on a given date.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct VersionDownloads {
    pub date: NaiveDate,
    pub downloads: u64,
    pub version: u64,
}

/// Extra download data not attributed to a specific version date.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct ExtraDownloads {
    pub date: NaiveDate,
    pub downloads: u64,
}

/// Metadata about extra downloads.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct CrateDownloadsMeta {
    pub extra_downloads: Vec<ExtraDownloads>,
}

/// Download data for all versions of a [`Crate`].
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct CrateDownloads {
    pub version_downloads: Vec<VersionDownloads>,
    pub meta: CrateDownloadsMeta,
}

/// A crates.io user.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct User {
    pub avatar: Option<String>,
    pub email: Option<String>,
    pub id: u64,
    pub kind: Option<String>,
    pub login: String,
    pub name: Option<String>,
    pub url: String,
}

/// Author names metadata.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct AuthorsMeta {
    pub names: Vec<String>,
}

/// Crate author names.
#[allow(missing_docs)]
pub struct Authors {
    /// List of author names.
    pub names: Vec<String>,
}

/// API response wrapper for author metadata.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthorsResponse {
    pub meta: AuthorsMeta,
}

/// API response wrapper for a single user.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct UserResponse {
    pub user: User,
}

/// Crate owners.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Owners {
    pub users: Vec<User>,
}

/// A crate dependency.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Dependency {
    pub crate_id: String,
    pub default_features: bool,
    pub downloads: u64,
    pub features: Vec<String>,
    pub id: u64,
    pub kind: String,
    pub optional: bool,
    pub req: String,
    pub target: Option<String>,
    pub version_id: u64,
}

/// List of dependencies of a crate version.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct Dependencies {
    pub dependencies: Vec<Dependency>,
}

/// A single reverse dependency (i.e. a dependent crate).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct ReverseDependency {
    pub crate_version: Version,
    pub dependency: Dependency,
}

/// Raw reverse-dependency response as received from the REST API.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct ReverseDependenciesAsReceived {
    pub dependencies: Vec<Dependency>,
    pub versions: Vec<Version>,
    pub meta: Meta,
}

/// Full list of reverse dependencies for a crate.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct ReverseDependencies {
    pub dependencies: Vec<ReverseDependency>,
    pub meta: Meta,
}

impl ReverseDependencies {
    pub(crate) fn extend(&mut self, rdeps: ReverseDependenciesAsReceived) {
        self.meta.total = rdeps.meta.total;
        let version_map: HashMap<u64, &Version> =
            rdeps.versions.iter().map(|v| (v.id, v)).collect();
        for d in &rdeps.dependencies {
            if let Some(v) = version_map.get(&d.version_id) {
                self.dependencies.push(ReverseDependency {
                    crate_version: (*v).clone(),
                    dependency: d.clone(),
                });
            }
        }
    }
}

/// Complete information for a crate version (including authors and dependencies).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct FullVersion {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dl_path: String,
    pub downloads: u64,
    pub features: HashMap<String, Vec<String>>,
    pub id: u64,
    pub num: String,
    pub yanked: bool,
    pub license: Option<String>,
    pub readme_path: Option<String>,
    pub links: VersionLinks,

    pub author_names: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

/// Complete information for a crate.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(missing_docs)]
pub struct FullCrate {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub documentation: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub total_downloads: u64,
    pub max_version: String,
    pub max_stable_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    pub categories: Vec<Category>,
    pub keywords: Vec<Keyword>,
    pub downloads: CrateDownloads,
    pub owners: Vec<User>,
    pub reverse_dependencies: ReverseDependencies,

    pub versions: Vec<FullVersion>,
}
