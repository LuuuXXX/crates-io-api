//! API client for [crates.io](https://crates.io) based on the **sparse registry index**.
//!
//! This crate provides the same public interface as the reference
//! `crates_io_api` library (which uses the crates.io REST web API) but
//! fetches crate/version data from the official **sparse registry index** at
//! `https://index.crates.io/` — the same data source used by Cargo for
//! dependency resolution.  Data that is not available in the index
//! (download stats, owners, authors, reverse dependencies, summary stats,
//! and user lookup) is automatically fetched from the crates.io REST API
//! at `https://crates.io/api/v1/` as a fallback.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │ AsyncClient / SyncClient                                            │
//! │                                                                     │
//! │  get_crate / crate_dependencies ──► sparse index (index.crates.io) │
//! │  summary / crate_downloads      ──► REST API     (crates.io/api)   │
//! │  crate_owners / crate_authors   ──► REST API     (crates.io/api)   │
//! │  crate_reverse_dependencies     ──► REST API     (crates.io/api)   │
//! │  user                           ──► REST API     (crates.io/api)   │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Why use this instead of the web API?
//!
//! * **Faster crate/version resolution** — the index endpoint is designed for
//!   automated bulk access by package managers.
//! * **Stable protocol** — the index format is part of the Cargo specification
//!   and changes are versioned.
//! * **Complete dependency data** — the full dependency graph for every
//!   published version is available without additional API calls.
//!
//! # Capability table
//!
//! | Field / method                  | Data source                      |
//! |---------------------------------|----------------------------------|
//! | Version list, features, yanked  | ✓ Sparse index                   |
//! | Dependency tree (per version)   | ✓ Sparse index                   |
//! | Max / max-stable version        | ✓ Derived from semver ordering   |
//! | Download counts                 | ✓ REST API fallback              |
//! | Owners / authors                | ✓ REST API fallback              |
//! | Reverse dependencies            | ✓ REST API fallback              |
//! | Summary statistics              | ✓ REST API fallback              |
//! | Crate search                    | ✓ Exact-name lookup (index)      |
//! | User lookup                     | ✓ REST API fallback              |
//!
//! # Rate limiting
//!
//! All clients enforce a configurable delay between consecutive HTTP requests.
//! The crates.io [Crawler Policy] recommends at least **1 second** between
//! requests.  Pass a [`std::time::Duration`] of at least 1 second as the
//! second argument to [`SyncClient::new`] or [`AsyncClient::new`].
//!
//! [Crawler Policy]: https://crates.io/policies#crawlers
//!
//! # Examples
//!
//! ## Synchronous
//!
//! ```rust,no_run
//! use crates_io_api::{SyncClient, Error};
//!
//! fn print_serde_deps() -> Result<(), Error> {
//!     let client = SyncClient::new(
//!         "my-tool (my-contact@example.com)",
//!         std::time::Duration::from_millis(1000),
//!     )?;
//!     let deps = client.crate_dependencies("serde", "1.0.193")?;
//!     for dep in deps {
//!         println!("  {} {}", dep.crate_id, dep.req);
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Asynchronous
//!
//! ```rust,no_run
//! use crates_io_api::{AsyncClient, Error};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Error> {
//!     let client = AsyncClient::new(
//!         "my-tool (my-contact@example.com)",
//!         std::time::Duration::from_millis(1000),
//!     )?;
//!     let krate = client.get_crate("serde").await?;
//!     println!("serde {}: {} versions",
//!         krate.crate_data.max_version,
//!         krate.versions.len());
//!     Ok(())
//! }
//! ```


#![deny(missing_docs)]

mod async_client;
mod convert;
mod error;
mod index;
mod sync_client;
mod types;

pub use crate::{
    async_client::Client as AsyncClient,
    error::{Error, NotFoundError, PermissionDeniedError},
    sync_client::SyncClient,
    types::*,
};
