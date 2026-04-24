//! Error types.

/// Errors returned by the API client.
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

/// Error returned when a resource could not be found.
#[derive(Debug)]
pub struct NotFoundError {
    pub(crate) url: String,
}

impl std::fmt::Display for NotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Resource at '{}' could not be found", self.url)
    }
}

/// Error returned when a resource is not accessible.
#[derive(Debug)]
pub struct PermissionDeniedError {
    pub(crate) reason: String,
}

impl std::fmt::Display for PermissionDeniedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Permission denied: {}", self.reason)
    }
}
