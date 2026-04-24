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
//! # Why use this instead of the web API?
//!
//! * **Faster crate/version resolution** — the index endpoint is designed for
//!   automated bulk access by package managers.
//! * **Stable protocol** — the index format is part of the Cargo specification
//!   and changes are versioned.
//! * **Complete dependency data** — the full dependency graph for every
//!   published version is available.
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
//! # Examples
//!
//! ```rust
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
