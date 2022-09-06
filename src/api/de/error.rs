use std::{borrow::Cow, fmt::Display};

#[derive(Debug)]
pub enum DeserializerError {
    MissingWord,
    MissingKey,
    MissingValue,
    BadPrimitiveValue(Box<dyn std::error::Error>),
    Custom(Cow<'static, str>),
}

impl DeserializerError {
    pub fn custom<T>(text: T) -> Self
    where
        T: Into<Cow<'static, str>>,
    {
        Self::Custom(text.into())
    }
}

impl Display for DeserializerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use DeserializerError::*;
        match self {
            MissingWord => f.write_str("missing word to finish the sentence"),
            MissingKey => f.write_str("failed to parse key from current word"),
            MissingValue => f.write_str("failed to parse value from current word"),

            BadPrimitiveValue(e) => e.fmt(f),

            Custom(msg) => f.write_str(msg.as_ref()),
        }
    }
}

impl std::error::Error for DeserializerError {}

impl serde::de::Error for DeserializerError {
    fn custom<T>(msg: T) -> Self
    where
        T: std::fmt::Display,
    {
        Self::custom(msg.to_string())
    }
}
