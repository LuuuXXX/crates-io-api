//! API client for [crates.io](https://crates.io) based on the **sparse registry index**.
//!
//! This crate provides the same public interface as the reference
//! `crates_io_api` library (which uses the crates.io REST web API) but
//! fetches data exclusively from the official **sparse registry index** at
//! `https://index.crates.io/` — the same data source used by Cargo for
//! dependency resolution.
//!
//! # Why use this instead of the web API?
//!
//! * **No rate-limit surprises** — the index endpoint is designed for
//!   automated bulk access by package managers.
//! * **Stable protocol** — the index format is part of the Cargo specification
//!   and changes are versioned.
//! * **Complete dependency data** — the full dependency graph for every
//!   published version is available.
//!
//! # Limitations
//!
//! The sparse index does not carry the richer metadata exposed by the REST
//! API.  Specifically:
//!
//! | Field / method                  | Status in this crate             |
//! |---------------------------------|----------------------------------|
//! | Version list, features, yanked  | ✓ Fully supported                |
//! | Dependency tree (per version)   | ✓ Fully supported                |
//! | Max / max-stable version        | ✓ Derived from semver ordering   |
//! | Download counts                 | ✗ Always 0                      |
//! | Owners / authors                | ✗ Always empty                  |
//! | Reverse dependencies            | ✗ Always empty                  |
//! | Summary statistics              | ✗ Always zero / empty           |
//! | Crate search                    | ✓ Exact-name lookup only        |
//! | User lookup                     | ✗ Returns `NotFound`            |
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
