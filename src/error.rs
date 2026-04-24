//! Error types for the crates.io API client.
//!
//! The central type is [`Error`], an enum that covers all failure modes that
//! can arise when calling the crates.io sparse registry index or the REST API.
//!
//! # Error handling
//!
//! ```rust,no_run
//! use crates_io_api::{SyncClient, Error, NotFoundError};
//!
//! fn lookup(name: &str) {
//!     let client = SyncClient::new("my-bot (bot@example.com)",
//!         std::time::Duration::from_secs(1)).unwrap();
//!     match client.get_crate(name) {
//!         Ok(resp) => println!("Found: {}", resp.crate_data.name),
//!         Err(Error::NotFound(_)) => println!("{name} not found in registry"),
//!         Err(e) => eprintln!("error: {e}"),
//!     }
//! }
//! ```

/// Errors returned by the API client.
///
/// This is a non-exhaustive enum; new variants may be added in future minor
/// releases without a breaking change.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Low-level HTTP error.
    Http(reqwest::Error),
    /// Invalid URL.
    Url(url::ParseError),
    /// Resource could not be found.
    NotFound(NotFoundError),
    /// No permission to access the resource.
    PermissionDenied(PermissionDeniedError),
    /// JSON decoding failed.
    JsonDecode(JsonDecodeError),
    /// Error returned by the crates.io API.
    Api(crate::types::ApiErrors),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => e.fmt(f),
            Error::Url(e) => e.fmt(f),
            Error::NotFound(e) => e.fmt(f),
            Error::PermissionDenied(e) => e.fmt(f),
            Error::Api(err) => {
                let inner = if err.errors.is_empty() {
                    "Unknown API error".to_string()
                } else {
                    err.errors
                        .iter()
                        .map(|err| err.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                write!(f, "API Error ({})", inner)
            }
            Error::JsonDecode(err) => write!(f, "Could not decode registry JSON: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::Url(e) => Some(e),
            Error::NotFound(_) => None,
            Error::PermissionDenied(_) => None,
            Error::Api(_) => None,
            Error::JsonDecode(err) => Some(err),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<url::ParseError> for Error {
    fn from(e: url::ParseError) -> Self {
        Error::Url(e)
    }
}

/// Error returned when JSON from the registry could not be decoded.
///
/// This can occur if the registry returns an unexpected response shape or if
/// the internal type definitions drift from the actual API schema.
#[derive(Debug)]
pub struct JsonDecodeError {
    pub(crate) message: String,
}

impl std::fmt::Display for JsonDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Could not decode JSON: {}", self.message)
    }
}

impl std::error::Error for JsonDecodeError {}

/// Error returned when a resource could not be found (HTTP 404).
///
/// The `url` field contains the URL that returned a 404 response.
#[derive(Debug)]
pub struct NotFoundError {
    pub(crate) url: String,
}

impl std::fmt::Display for NotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Resource at '{}' could not be found", self.url)
    }
}

/// Error returned when a resource is not accessible (HTTP 403).
///
/// The `reason` field may contain the body of the error response from
/// the server.
#[derive(Debug)]
pub struct PermissionDeniedError {
    pub(crate) reason: String,
}

impl std::fmt::Display for PermissionDeniedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Permission denied: {}", self.reason)
    }
}
