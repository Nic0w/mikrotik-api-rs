use std::{
    io::{self, Cursor},
    marker::PhantomData,
};

use bytes::Buf;
use serde::{
    de::{self, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, VariantAccess, Visitor},
    forward_to_deserialize_any, Deserializer,
};

use super::Error;

pub struct ApiDeserializer<'de> {
    cursor: &'de mut Cursor<&'de [u8]>,

    current_word: Option<&'de str>,
}

impl<'de> ApiDeserializer<'de> {
    pub fn new(cursor: &'de mut Cursor<&'de [u8]>) -> Self {
        ApiDeserializer {
            cursor,
            current_word: None,
        }
    }

    pub fn inner(&self) -> &Cursor<&[u8]> {
        self.cursor
    }
}

impl<'de> ApiDeserializer<'de> {
    fn get_byte(&mut self) -> Option<u8> {
        self.cursor.has_remaining().then(|| self.cursor.get_u8())
    }

    fn read_len(&mut self) -> Result<u32, Error> {
        let mut next_byte = || self.get_byte().ok_or(Error::Incomplete);

        let first_byte = next_byte()?;

        if first_byte >> 7 == 0b0 {
            return Ok(first_byte as u32);
        }

        let mut data: [u8; 4] = [0; 4];

        if first_byte >> 6 == 0b10 {
            data[0] = first_byte & !0xC0;
            data[1] = next_byte()?;

            return Ok(u32::from_ne_bytes(data));
        }

        if first_byte >> 5 == 0b110 {
            data[0] = first_byte & !0xE0;
            data[1] = next_byte()?;
            data[2] = next_byte()?;

            return Ok(u32::from_ne_bytes(data));
        }

        if first_byte >> 4 == 0b1110 {
            data[0] = first_byte & !0xF0;
            data[1] = next_byte()?;
            data[2] = next_byte()?;
            data[3] = next_byte()?;

            return Ok(u32::from_ne_bytes(data));
        }

        if first_byte == 0xF0 {
            data[0] = next_byte()?;
            data[1] = next_byte()?;
            data[2] = next_byte()?;
            data[3] = next_byte()?;

            return Ok(u32::from_ne_bytes(data));
        }

        /*let masks = [0x80, 0xC0, 0xE0, 0xF0].into_iter();

        for (bytes, mask) in masks.enumerate() {
            let len = bytes + 1;

            if first_byte & mask == mask {

                let mut data: [u8; 4] = [0; 4];

                data[0] = first_byte;

                get_bytes(src, &mut data[1..len])?;

                return Ok(u32::from_ne_bytes(data) & ((mask as u32) << (len*8)) );
            }
        }*/
        unreachable!()
    }

    fn read_bytes(&mut self, len: u32) -> Result<&'de [u8], Error> {
        let start = self.cursor.position() as usize;
        let end = self.cursor.get_ref().len();

        let remaining = end - start;

        if len > (remaining as u32) {
            return Err(Error::Incomplete);
        }

        self.cursor.set_position((start + len as usize) as u64);

        Ok(&self.cursor.get_ref()[start..start + (len as usize)])
    }

    fn read_word(&mut self) -> Result<&'de str, Error> {
        let str_len = self.read_len()?;

        let str_bytes = self.read_bytes(str_len)?;

        let text = unsafe { core::str::from_utf8_unchecked(str_bytes) };

        Ok(text)
    }

    fn word_part(&mut self, hint: Hint) -> Result<&'de str, Error> {
        if let Some(text) = self.current_word {
            use Hint::*;
            match hint {
                Key => {
                    let (key, _) = text[1..].split_once('=').ok_or(Error::Incomplete)?;

                    Ok(key)
                }

                Value => {
                    let (_, value) = text[1..].split_once('=').ok_or(Error::Incomplete)?;

                    Ok(value)
                }
            }
        } else {
            Err(Error::Incomplete)
        }
    }
}

impl std::fmt::Display for super::Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl std::error::Error for super::Error {}

impl serde::de::Error for super::Error {
    fn custom<T>(msg: T) -> Self
    where
        T: std::fmt::Display,
    {
        let msg = msg.to_string();

        let err = io::Error::new(io::ErrorKind::Other, msg);

        Error::Other(err)
    }
}

impl<'de, 'api> Deserializer<'de> for &'api mut ApiDeserializer<'de> {
    type Error = super::Error;

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u128 f32 f64 char
        bytes byte_buf option unit_struct newtype_struct tuple
        tuple_struct map seq
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        unimplemented!("deserialize_any")
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>{


    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("deserialize_struct: {:?}", self.current_word);

        // !done
        //self.current_word = Some(self.read_word()?);

        visitor.visit_map(StructVisitor { de: self })

        //unimplemented!("deserialize_struct: {}->{:?}", name, fields)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("deserialize_string: {:?}", self.current_word);

        let text = self.word_part(Hint::Value)?;

        visitor.visit_borrowed_str(text)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("deserialize_identifier: {:?}", self.current_word);

        match self.current_word {
            Some("!done") => visitor.visit_borrowed_str("Done"),
            Some("!re") => visitor.visit_borrowed_str("Reply"),
            Some("!trap") => visitor.visit_borrowed_str("Trap"),
            Some("!fatal") => visitor.visit_borrowed_str("Fatal"),

            Some(_) => {
                let text = self.word_part(Hint::Key)?;
                visitor.visit_borrowed_str(text)
            }

            _ => Err(Error::Incomplete),
        }
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("deserialize_enum: {:?}", self.current_word);

        self.current_word = Some(self.read_word()?);

        match self.current_word {
            Some("!done") | Some("!re") | Some("!trap") | Some("!fatal") => {
                visitor.visit_enum(EnumVisitor { de: self })
            }

            Some(variant) => visitor.visit_enum(variant.into_deserializer()),

            _ => Err(Error::Incomplete),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("deserialize_str: {:?}", self.current_word);

        let text = self.word_part(Hint::Value)?;

        visitor.visit_borrowed_str(text)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.read_word()?;

        visitor.visit_unit()
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }
}

struct SeqVisitor<'v, 'de: 'v> {
    pub de: &'v mut ApiDeserializer<'de>,
}

impl<'de, 'v> SeqAccess<'de> for SeqVisitor<'v, 'de> {
    type Error = Error;

    fn next_element_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>, Self::Error>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        println!("plop");

        self.de.current_word = Some(self.de.read_word()?);

        if let Some("") = self.de.current_word {
            println!("pliop");

            return Ok(None);
        }

        seed.deserialize(&mut *self.de).map(Some)
    }
}

enum Hint {
    Key,
    Value,
}

struct StructVisitor<'v, 'de: 'v> {
    pub de: &'v mut ApiDeserializer<'de>,
}

impl<'de, 'v> MapAccess<'de> for StructVisitor<'v, 'de> {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        println!("gné");

        self.de.current_word = Some(self.de.read_word()?);

        if let Some("") = self.de.current_word {
            println!("pliup");

            return Ok(None);
        }

        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        println!("next_value_seed: {:?}", self.de.current_word);

        seed.deserialize(&mut *self.de)
    }
}

struct EnumVisitor<'v, 'de: 'v> {
    pub de: &'v mut ApiDeserializer<'de>,
}

impl<'de, 'v> EnumAccess<'de> for EnumVisitor<'v, 'de> {
    type Error = Error;

    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        println!("prout");

        let val = seed.deserialize(&mut *self.de)?;

        //self.de.current_word = Some(self.de.read_word()?);

        // Parse the colon separating map key from value.
        if self.de.current_word.is_some() {
            println!("prozzzut");

            Ok((val, self))
        } else {
            println!("proiiiiut");

            Err(Error::Incomplete)
        }
    }
}

impl<'de, 'v> VariantAccess<'de> for EnumVisitor<'v, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        todo!("unit_variant");
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        println!("newtype_variant_seed");
        seed.deserialize(self.de)
    }

    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!("tuple_variant")
    }

    fn struct_variant<V>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        println!("struct_variant");

        de::Deserializer::deserialize_struct(self.de, "", fields, visitor)
    }
}

fn copy_bytes<'b>(src: &mut Cursor<&'b [u8]>, dest: &mut [u8]) -> Result<(), Error> {
    let start = src.position() as usize;
    let end = src.get_ref().len();

    let remaining = end - start;

    let buf_len = dest.len();

    if buf_len > remaining {
        return Err(Error::Incomplete);
    }

    dest.copy_from_slice(&src.get_ref()[start..start + buf_len]);

    Ok(())
}
