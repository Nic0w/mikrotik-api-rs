use std::{slice::Iter, str::FromStr};

use serde::{
    de::{self, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, VariantAccess, Visitor},
    forward_to_deserialize_any, Deserializer,
};

use super::Response;

mod error;

pub use error::DeserializerError;

type Result<T> = std::result::Result<T, error::DeserializerError>;

pub fn deserialize_sentence<T: de::DeserializeOwned>(sentence: &[String]) -> Result<Response<T>> {
    let mut iterator = sentence.iter();

    let mut deserializer = SentenceDeserializer::new(&mut iterator);

    use serde::Deserialize;

    <Response<T>>::deserialize(&mut deserializer)
}

pub struct SentenceDeserializer<'de> {
    cursor: &'de mut Iter<'de, String>,

    current_word: Option<&'de str>,
}

impl<'de> SentenceDeserializer<'de> {
    pub fn new(iter: &'de mut Iter<'de, String>) -> Self {
        //println!("{:?}", iter);

        SentenceDeserializer {
            cursor: iter,
            current_word: None,
        }
    }

    pub fn inner(&self) -> &Iter<String> {
        self.cursor
    }
}

impl<'de> SentenceDeserializer<'de> {
    fn read_word(&mut self) -> Result<&'de String> {
        let next = self.cursor.next().ok_or(DeserializerError::MissingWord)?;

        if next.starts_with(".tag") {
            return self.read_word();
        }

        Ok(next)
    }

    fn word_part(&mut self) -> Result<&'de str> {
        if let Some(text) = self.current_word.as_mut() {

            let mut matches = text.match_indices('=');

            let first = matches.next();
            let second = matches.next();

            if let Some((i, _)) = second {
                let split = text.split_at(i);

                *text = split.1;

                Ok(&split.0[1..])
            } 
            else if first.is_some() {
                let (empty, value) = text.split_at(0);

                *text = empty;

                Ok(&value[1..])
            }
            else {
                Err(DeserializerError::MissingWord)
            }
        } else {
            Err(DeserializerError::MissingWord)
        }
    }

    fn parse_unsigned<T>(&mut self) -> Result<T>
    where
        T: FromStr + From<u8>,
        T::Err: std::error::Error + 'static,
    {
        let text = self.word_part()?;

        text.parse().map_err(|e| {
            DeserializerError::BadPrimitiveValue(Box::<dyn std::error::Error>::from(e))
        })
    }
}

impl<'de, 'api> Deserializer<'de> for &'api mut SentenceDeserializer<'de> {
    type Error = DeserializerError;

    forward_to_deserialize_any! {
        i8 i16 i32 i64 i128 u128 f32 f64 char
        bytes byte_buf unit_struct newtype_struct tuple
        tuple_struct seq
    }

    fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        unimplemented!("deserialize_any");
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_map(StructVisitor { de: self })
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let text = self.word_part()?;

        visitor.visit_borrowed_str(text)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.current_word {
            Some("!done") => visitor.visit_borrowed_str("Done"),
            Some("!re") => visitor.visit_borrowed_str("Reply"),
            Some("!trap") => visitor.visit_borrowed_str("Trap"),
            Some("!fatal") => visitor.visit_borrowed_str("Fatal"),

            Some(_) => {
                let text = self.word_part()?;
                visitor.visit_borrowed_str(text)
            }

            _ => Err(DeserializerError::MissingWord),
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.current_word = Some(self.read_word()?);

        match self.current_word {
            Some("!done") => visitor.visit_enum("Done".into_deserializer()),

            Some("!re") | Some("!trap") | Some("!fatal") => {
                visitor.visit_enum(EnumVisitor { de: self })
            }

            Some(variant) => visitor.visit_enum(variant.into_deserializer()),

            _ => Err(DeserializerError::MissingWord),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let text = self.word_part()?;

        visitor.visit_borrowed_str(text)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u64(self.parse_unsigned()?)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u8(self.parse_unsigned()?)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u16(self.parse_unsigned()?)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u32(self.parse_unsigned()?)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_some(self)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_map(StructVisitor { de: self })
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.word_part()? {
            "true" => visitor.visit_bool(true),
            "false" => visitor.visit_bool(false),
            e => Err(DeserializerError::BadPrimitiveValue(Box::<
                dyn std::error::Error,
            >::from(e))),
        }
    }
}

struct SeqVisitor<'v, 'de: 'v> {
    pub de: &'v mut SentenceDeserializer<'de>,
}

impl<'de, 'v> SeqAccess<'de> for SeqVisitor<'v, 'de> {
    type Error = DeserializerError;

    fn next_element_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        self.de.current_word = Some(self.de.read_word()?);

        if let Some("") = self.de.current_word {
            return Ok(None);
        }

        seed.deserialize(&mut *self.de).map(Some)
    }
}

#[derive(Debug)]
enum Hint {
    Key,
    Value,
}

struct StructVisitor<'v, 'de: 'v> {
    pub de: &'v mut SentenceDeserializer<'de>,
}

impl<'de, 'v> MapAccess<'de> for StructVisitor<'v, 'de> {
    type Error = DeserializerError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        self.de.current_word = Some(self.de.read_word()?);

        if let Some("") = self.de.current_word {
            return Ok(None);
        }

        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(&mut *self.de)
    }
}

struct EnumVisitor<'v, 'de: 'v> {
    pub de: &'v mut SentenceDeserializer<'de>,
}

impl<'de, 'v> EnumAccess<'de> for EnumVisitor<'v, 'de> {
    type Error = DeserializerError;

    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant)>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let val = seed.deserialize(&mut *self.de)?;

        // Parse the colon separating map key from value.
        if self.de.current_word.is_some() {
            Ok((val, self))
        } else {
            Err(DeserializerError::MissingWord)
        }
    }
}

impl<'de, 'v> VariantAccess<'de> for EnumVisitor<'v, 'de> {
    type Error = DeserializerError;

    fn unit_variant(self) -> Result<()> {
        todo!("EnumVisitor::unit_variant")
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(self.de)
    }

    fn tuple_variant<V>(self, _len: usize, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        todo!("EnumVisitor::tuple_variant")
    }

    fn struct_variant<V>(self, fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        de::Deserializer::deserialize_struct(self.de, "", fields, visitor)
    }
}
