use std::iter::FromIterator;

use serde::{
    de::{self, Visitor},
    Deserialize,
};

use super::error::Error;

#[derive(Debug, Deserialize)]
pub enum Response<T> {
    Done,

    Reply(T),

    Trap {
        category: Option<u32>,
        message: String,
    },
    Fatal,
}

impl<T> From<Response<T>> for Result<T, Error> {
    fn from(response: Response<T>) -> Self {
        match response {
            Response::Reply(value) => Ok(value),
            Response::Trap { message, .. } => Err(Error::Remote(message)),
            _ => unreachable!(),
        }
    }
}

impl<A, V: FromIterator<A>> FromIterator<Response<A>> for Response<V> {
    fn from_iter<T: IntoIterator<Item = Response<A>>>(iter: T) -> Self {
        let mut found_trap = None;

        use Response::*;
        //No idea what I'm doing. This code has been inspired from https://github.com/rust-lang/rust/pull/59605
        let v: V = FromIterator::from_iter(iter.into_iter().scan((), |_, elt| match elt {
            Done | Fatal => None,
            Reply(value) => Some(value),

            trap @ Trap { .. } => {
                found_trap = Some(trap);
                None
            }
        }));

        match found_trap {
            Some(Trap { message, category }) => Trap { category, message },

            None => Reply(v),

            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SystemResources {
    pub uptime: String,
    pub version: String,
    pub build_time: String,
    pub factory_software: String,

    pub free_memory: u32,
    pub total_memory: u32,

    pub cpu: String,
    pub cpu_count: u8,
    pub cpu_load: u16,

    pub free_hdd_space: u32,
    pub total_hdd_space: u32,

    pub architecture_name: String,
    pub board_name: String,
    pub platform: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ActiveUserRaw {
    #[serde(rename = ".id")]
    pub id: String,

    #[serde(rename = ".dead")]
    #[serde(default)]
    pub is_dead: bool,

    pub when: Option<String>,
    pub name: Option<String>,
    pub address: Option<String>,
    pub via: Option<String>,
    pub group: Option<String>,
    pub radius: Option<bool>,
}

#[derive(Debug)]
pub enum ActiveUser {
    Dead(String),

    Active {
        id: String,
        when: String,
        name: String,
        address: String,
        via: String,
        group: String,
        radius: bool,
    },
}

impl<'de> Deserialize<'de> for ActiveUser {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ActiveUserRaw::deserialize(deserializer)?;

        if raw.is_dead {
            return Ok(ActiveUser::Dead(raw.id));
        }

        let id = raw.id;
        let when = raw.when.ok_or_else(|| de::Error::missing_field("when"))?;
        let name = raw.name.ok_or_else(|| de::Error::missing_field("name"))?;
        let address = raw
            .address
            .ok_or_else(|| de::Error::missing_field("address"))?;
        let via = raw.via.ok_or_else(|| de::Error::missing_field("via"))?;
        let group = raw.group.ok_or_else(|| de::Error::missing_field("group"))?;
        let radius = raw
            .radius
            .ok_or_else(|| de::Error::missing_field("radius"))?;

        Ok(ActiveUser::Active {
            id,
            when,
            name,
            address,
            via,
            group,
            radius,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct InterfaceChange {
    #[serde(rename = ".id")]
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Interface {
    #[serde(rename = ".id")]
    pub id: String,

    pub name: String,

    #[serde(rename = "type")]
    pub iface_type: String,

    pub mtu: InterfaceMTU,
    pub actual_mtu: u16,

    pub last_link_up: Option<String>,
    pub link_downs: u32,

    pub rx_byte: u64,
    pub tx_byte: u64,
    pub rx_packet: u64,
    pub tx_packet: u64,
    pub rx_drop: Option<u64>,
    pub tx_drop: Option<u64>,
    pub tx_queue_drop: u64,
    pub rx_error: Option<u64>,
    pub tx_error: Option<u64>,
    pub fp_rx_byte: u64,
    pub fp_tx_byte: u64,
    pub fp_rx_packet: u64,
    pub fp_tx_packet: u64,

    pub running: bool,
    #[serde(default)]
    pub slave: bool,

    pub disabled: bool,
}

#[derive(Debug)]
pub enum InterfaceMTU {
    Auto,
    Value(u16),
}

impl<'de> Deserialize<'de> for InterfaceMTU {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BorrowedString;
        impl<'de> Visitor<'de> for BorrowedString {
            type Value = &'de str;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a valid number or 'auto'")
            }

            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(v)
            }
        }

        match deserializer.deserialize_str(BorrowedString)? {
            "auto" => Ok(InterfaceMTU::Auto),

            other => other.parse::<u16>().map(InterfaceMTU::Value).map_err(|_| {
                de::Error::invalid_value(de::Unexpected::Str(other), &"'auto' or a valid number")
            }),
        }
    }
}

//["!re", ".tag=56795", "=.id=*40", "=name=wg1", "=type=wg", "=mtu=1420", "=actual-mtu=1420", "=last-link-up-time=sep/02/2022 10:32:48", "=link-downs=0", "=rx-byte=20832052", "=tx-byte=21330320", "=rx-packet=127042", "=tx-packet=131803", "=rx-drop=0", "=tx-drop=5768", "=tx-queue-drop=0", "=rx-error=0", "=tx-error=104", "=fp-rx-byte=0", "=fp-tx-byte=0", "=fp-rx-packet=0", "=fp-tx-packet=0", "=running=true", "=disabled=false", ""]