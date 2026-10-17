use std::{slice::Iter, str::FromStr};

use serde::{
    de::{self, EnumAccess, IntoDeserializer, MapAccess, VariantAccess, Visitor},
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
    current_key: Option<&'de str>,
    current_value: Option<&'de str>,
}

impl<'de> SentenceDeserializer<'de> {
    pub fn new(iter: &'de mut Iter<'de, String>) -> Self {
        //println!("{:?}", iter);

        SentenceDeserializer {
            cursor: iter,
            current_word: None,
            current_key: None,
            current_value: None,
        }
    }

    pub fn inner(&self) -> &Iter<'de, String> {
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

    fn set_key_value(&mut self, word: &'de str) {
        self.current_word = None;
        let trimmed = word.strip_prefix('=').unwrap_or(word);
        if let Some((key, value)) = trimmed.split_once('=') {
            self.current_key = Some(key);
            self.current_value = Some(value);
        } else {
            self.current_key = Some(trimmed);
            self.current_value = Some("");
        }
    }

    fn word_part(&mut self) -> Result<&'de str> {
        if let Some(key) = self.current_key.take() {
            Ok(key)
        } else if let Some(value) = self.current_value.take() {
            Ok(value)
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

impl<'de> Deserializer<'de> for &mut SentenceDeserializer<'de> {
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

            _ => {
                let text = self.word_part()?;
                visitor.visit_borrowed_str(text)
            }
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
        let word = self.read_word()?;
        self.current_word = Some(word);

        match word.as_str() {
            "!done" => visitor.visit_enum("Done".into_deserializer()),

            "!re" | "!trap" | "!fatal" => visitor.visit_enum(EnumVisitor { de: self }),

            variant => visitor.visit_enum(variant.into_deserializer()),
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
        let _ = self.current_value.take();
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
            "true" | "yes" => visitor.visit_bool(true),
            "false" | "no" => visitor.visit_bool(false),
            e => Err(DeserializerError::BadPrimitiveValue(Box::<
                dyn std::error::Error,
            >::from(e))),
        }
    }
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
        let word = self.de.read_word()?;

        if word.is_empty() {
            return Ok(None);
        }

        self.de.set_key_value(word);

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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::Deserialize;

    use super::deserialize_sentence;
    use crate::api::model::{Response, TrapCategory};

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "kebab-case")]
    struct FirewallRule {
        name: String,
        comment: String,
        packet_count: u32,
        enabled: bool,
    }

    #[test]
    fn test_deserialize_value_containing_equals_signs() {
        let sentence = vec![
            "!re".to_string(),
            ".tag=12".to_string(),
            "=name=drop-wan-ssh".to_string(),
            "=comment=proto=TCP port=22 src-mac=AA:BB:CC:DD:EE:FF in:wan out:lan".to_string(),
            "=packet-count=42".to_string(),
            "=enabled=true".to_string(),
            "".to_string(),
        ];

        let res: Response<FirewallRule> =
            deserialize_sentence(&sentence).expect("failed to deserialize sentence");

        match res {
            Response::Reply(rule) => {
                assert_eq!(rule.name, "drop-wan-ssh");
                assert_eq!(
                    rule.comment,
                    "proto=TCP port=22 src-mac=AA:BB:CC:DD:EE:FF in:wan out:lan"
                );
                assert_eq!(rule.packet_count, 42);
                assert!(rule.enabled);
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn test_deserialize_base64_and_empty_values_with_equals() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct TokenInfo {
            token: String,
            signature: String,
            empty_val: String,
            consecutive: String,
        }

        let sentence = vec![
            "!re".to_string(),
            "=token=eyJhbGciOiJIUzI1NiJ9.test.sig==".to_string(),
            "=signature=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA==".to_string(),
            "=empty-val=".to_string(),
            "=consecutive=====".to_string(),
            "".to_string(),
        ];

        let res: Response<TokenInfo> =
            deserialize_sentence(&sentence).expect("failed to deserialize base64");

        match res {
            Response::Reply(info) => {
                assert_eq!(info.token, "eyJhbGciOiJIUzI1NiJ9.test.sig==");
                assert_eq!(
                    info.signature,
                    "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA=="
                );
                assert_eq!(info.empty_val, "");
                assert_eq!(info.consecutive, "====");
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn test_deserialize_into_hashmap_with_equals_values() {
        let sentence = vec![
            "!re".to_string(),
            "=key1=val1".to_string(),
            "=script=:if ($a = $b) do={ /ip firewall print }".to_string(),
            "=filter=?name=ether1&running=true".to_string(),
            "".to_string(),
        ];

        let res: Response<HashMap<String, String>> =
            deserialize_sentence(&sentence).expect("failed to deserialize HashMap");

        match res {
            Response::Reply(map) => {
                assert_eq!(map.get("key1").unwrap(), "val1");
                assert_eq!(
                    map.get("script").unwrap(),
                    ":if ($a = $b) do={ /ip firewall print }"
                );
                assert_eq!(map.get("filter").unwrap(), "?name=ether1&running=true");
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn test_deserialize_trap_message_with_equals() {
        let sentence = vec![
            "!trap".to_string(),
            "=category=1".to_string(),
            "=message=syntax error at line 3: where ($status = \"active\")".to_string(),
            "".to_string(),
        ];

        let res: Response<()> =
            deserialize_sentence(&sentence).expect("failed to deserialize Trap");

        match res {
            Response::Trap { category, message } => {
                assert_eq!(category, Some(TrapCategory::ArgumentValueFailure));
                assert_eq!(
                    message,
                    "syntax error at line 3: where ($status = \"active\")"
                );
            }
            other => panic!("expected Trap, got {:?}", other),
        }
    }

    #[test]
    fn test_deserialize_boolean_yes_no() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct Flags {
            flag_yes: bool,
            flag_no: bool,
            flag_true: bool,
            flag_false: bool,
        }

        let sentence = vec![
            "!re".to_string(),
            "=flag-yes=yes".to_string(),
            "=flag-no=no".to_string(),
            "=flag-true=true".to_string(),
            "=flag-false=false".to_string(),
            "".to_string(),
        ];

        let res: Response<Flags> =
            deserialize_sentence(&sentence).expect("failed to deserialize bools");

        match res {
            Response::Reply(flags) => {
                assert!(flags.flag_yes);
                assert!(!flags.flag_no);
                assert!(flags.flag_true);
                assert!(!flags.flag_false);
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }
}
