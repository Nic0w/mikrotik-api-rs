use std::fmt::Debug;
use std::io;

use super::de::DeserializerError;

/// Top-level error enum representing errors that can occur while communicating with RouterOS.
#[derive(Debug)]
pub enum Error {
    /// Incomplete word or sentence read from the socket, needing more bytes.
    Incomplete,
    /// Connection reached end of stream (EOF) or was terminated.
    EndOfStream,
    /// Router returned an empty response (!done without data) when data was expected.
    EmptyResponse,
    /// Error message returned remotely by the router inside a `!trap` sentence.
    Remote(String),
    /// Standard I/O error occurred during network communication.
    Io(io::Error),
    /// Invalid length prefix byte encountered on the wire.
    InvalidWordLength(u8),
    /// Failed to deserialize a router sentence into the target Rust type.
    Deserialize(DeserializerError),
    /// TLS configuration or handshake error.
    Tls(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<DeserializerError> for Error {
    fn from(e: DeserializerError) -> Self {
        Error::Deserialize(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Error::*;
        match self {
            Incomplete => f.write_str("not enough data"),

            EndOfStream => f.write_str("reached EOF"),

            EmptyResponse => f.write_str("empty response from router"),

            Remote(msg) => write!(f, "error from router: {}", msg),

            Io(e) => std::fmt::Display::fmt(&e, f),

            InvalidWordLength(b) => write!(f, "invalid word length prefix byte: {:#04x}", b),

            Deserialize(e) => write!(f, "deserialization error: {}", e),

            Tls(msg) => write!(f, "tls error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}
