use std::iter::FromIterator;

use serde::{
    de::{self, Visitor},
    Deserialize,
};

use super::error::Error;

/// A response to a command, sent by the router.
#[derive(Debug, Deserialize)]
pub enum Response<T> {
    /// `!done` sentence, indicating end of reply or stream
    Done,

    /// `!re` sentence, ie a reply from the router
    Reply(T),

    /// `!trap` sentence, sent by the router instead of `!re` when an error happened.
    Trap {
        /// Type of error
        /// TODO: make an enum, as possible values are well-known
        category: Option<TrapCategory>,

        /// Error message, to be shown to the user
        message: String,
    },
    /// `!fatal` sentence. A !fatal word is succeded by a simple string being the error message.
    Fatal,
}

/// Possible values for !trap `category`.
/// From https://wiki.mikrotik.com/wiki/Manual:API#category
#[derive(Debug)]
#[repr(u8)]
pub enum TrapCategory {
    /// 0 - missing item or command
    MissingItemOrCommand = 0,

    /// 1 - argument value failure
    ArgumentValueFailure = 1,

    /// 2 - execution of command interrupted
    CommandExecutionInterrupted = 2,

    /// 3 - scripting related failure
    ScriptingFailure = 3,

    /// 4 - general failure
    GeneralFailure = 4,

    /// 5 - API related failure
    APIFailure = 5,

    /// 6 - TTY related failure
    TTYFailure = 6,

    /// 7 - value generated with :return command
    ReturnValue = 7,
}

impl<'de> Deserialize<'de> for TrapCategory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match u8::deserialize(deserializer)? {
            //Safe because enum is repr(u8) and range is valid (from 0 to 7 inclusive)
            category @ 0..=7 => unsafe { Ok(core::mem::transmute(category)) },

            unknown => Err(de::Error::invalid_value(
                serde::de::Unexpected::Unsigned(unknown.into()),
                &"a known trap category",
            )),
        }
    }
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


/// Reply from `/system/resource/print` command
#[allow(missing_docs)]
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

#[allow(missing_docs)]
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

/// An event to describe user activity in terms of logins and logouts
#[derive(Debug)]
pub enum ActiveUser {

    /// Logout event, the String being the relative id of the user who logged out.
    Dead(String),

    /// Login event
    Active {
        /// Relative, incremental user id
        id: String,

        /// Login time
        when: String,

        /// Username
        name: String,

        /// IP address from which the connection originates from
        address: String,

        /// Mean of accessing admin interface: ssh, web, ...
        via: String,

        /// User group, as in which rights the user has on the system
        group: String,

        /// Is the user authenticated through RADIUS (?)
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


/// Incomplete reply from `/system/interface/listen` command
/// That may in fact be the same struct as `Interface`
#[allow(missing_docs)]
#[derive(Debug, Deserialize)]
pub struct InterfaceChange {
    #[serde(rename = ".id")]
    pub id: String,
}


/// Reply from `/system/interface/print` command
#[allow(missing_docs)]
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


/// Enum to represent the `mtu` field that can take either a number value or a text: 'auto'
#[derive(Debug)]
pub enum InterfaceMTU {
    /// 'auto' value
    Auto,

    /// MTU value in bytes
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