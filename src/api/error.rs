use std::fmt::Debug;
use std::io;

#[derive(Debug)]
pub enum Error {
    Incomplete,
    EndOfStream,
    Remote(String),
    Io(io::Error),
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

            Remote(msg) => write!(f, "error from router: {}", msg),

            Io(e) => std::fmt::Display::fmt(&e, f),
        }
    }
}

impl std::error::Error for Error {}
