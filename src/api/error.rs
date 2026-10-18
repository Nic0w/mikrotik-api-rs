use std::fmt::Debug;
use std::io;

#[derive(Debug)]
pub enum Error {
    Incomplete,
    EndOfStream,
    EmptyResponse,
    Remote(String),
    Io(io::Error),
    InvalidWordLength(u8),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
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
        }
    }
}

impl std::error::Error for Error {}
