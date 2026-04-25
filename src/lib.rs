//! API client for [crates.io](https://crates.io).
//!
//! This implementation fetches crate metadata from the **crates.io sparse
//! registry index** (`https://index.crates.io/`) instead of the JSON web API.
//! It is a drop-in replacement for the original `crates_io_api` crate for the
//! most commonly used operations.
//!
//! # Key differences from the original crate
//!
//! | Feature                  | Original (web API)  | This crate (sparse index) |
//! |--------------------------|---------------------|---------------------------|
//! | `get_crate`              | ✅                  | ✅                        |
//! | `crate_dependencies`     | ✅                  | ✅                        |
//! | `crate_downloads`        | ✅                  | ⚠️ empty (unavailable)    |
//! | `crate_owners`           | ✅                  | ⚠️ empty (unavailable)    |
//! | `crate_authors`          | ✅                  | ⚠️ empty (unavailable)    |
//! | `crate_reverse_deps`     | ✅                  | ⚠️ empty (unavailable)    |
//! | `full_crate`             | ✅                  | ✅ partial                |
//! | `summary` / `crates`     | ✅                  | ❌ returns `Error::Api`   |
//! | `user`                   | ✅                  | ❌ returns `Error::Api`   |
//! | Rate-limited per policy  | required            | respected                 |
//! | No crates.io web API     | ❌                  | ✅                        |
//!
//! # Quick start
//!
//! ```rust
//! use crates_io_api::{SyncClient, Error};
//!
//! fn check_crate_exists(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
//!     let client = SyncClient::new(
//!         "my-bot (contact@example.com)",
//!         std::time::Duration::from_millis(1000),
//!     )?;
//!     match client.get_crate(name) {
//!         Ok(_) => Ok(true),
//!         Err(Error::NotFound(_)) => Ok(false),
//!         Err(e) => Err(e.into()),
//!     }
//! }
//! ```

#![recursion_limit = "128"]
#![deny(missing_docs)]

mod async_client;
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
