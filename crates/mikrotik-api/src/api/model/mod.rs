use std::iter::FromIterator;

use serde::{
    de::{self, Visitor},
    Deserialize,
};

use super::error::Error;

/// A response to a command, sent by the router.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub enum Response<T> {
    /// `!done` sentence, indicating end of reply or stream
    Done,

    /// `!re` sentence, ie a reply from the router
    Reply(T),

    /// `!trap` sentence, sent by the router instead of `!re` when an error happened.
    Trap {
        /// Type of error
        #[serde(default)]
        category: Option<TrapCategory>,

        /// Error message, to be shown to the user
        #[serde(default)]
        message: String,
    },
    /// `!fatal` sentence. A !fatal word is succeded by a simple string being the error message.
    Fatal,
}

/// Possible values for !trap `category`.
/// From <https://wiki.mikrotik.com/wiki/Manual:API#category>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCategory {
    /// 0 - missing item or command
    MissingItemOrCommand,

    /// 1 - argument value failure
    ArgumentValueFailure,

    /// 2 - execution of command interrupted
    CommandExecutionInterrupted,

    /// 3 - scripting related failure
    ScriptingFailure,

    /// 4 - general failure
    GeneralFailure,

    /// 5 - API related failure
    APIFailure,

    /// 6 - TTY related failure
    TTYFailure,

    /// 7 - value generated with :return command
    ReturnValue,

    /// Unrecognized or future RouterOS trap category code
    Other(u8),
}

impl<'de> Deserialize<'de> for TrapCategory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match u8::deserialize(deserializer)? {
            0 => Ok(TrapCategory::MissingItemOrCommand),
            1 => Ok(TrapCategory::ArgumentValueFailure),
            2 => Ok(TrapCategory::CommandExecutionInterrupted),
            3 => Ok(TrapCategory::ScriptingFailure),
            4 => Ok(TrapCategory::GeneralFailure),
            5 => Ok(TrapCategory::APIFailure),
            6 => Ok(TrapCategory::TTYFailure),
            7 => Ok(TrapCategory::ReturnValue),
            unknown => Ok(TrapCategory::Other(unknown)),
        }
    }
}

impl From<TrapCategory> for u8 {
    fn from(category: TrapCategory) -> Self {
        match category {
            TrapCategory::MissingItemOrCommand => 0,
            TrapCategory::ArgumentValueFailure => 1,
            TrapCategory::CommandExecutionInterrupted => 2,
            TrapCategory::ScriptingFailure => 3,
            TrapCategory::GeneralFailure => 4,
            TrapCategory::APIFailure => 5,
            TrapCategory::TTYFailure => 6,
            TrapCategory::ReturnValue => 7,
            TrapCategory::Other(code) => code,
        }
    }
}

impl<T> From<Response<T>> for Result<T, Error> {
    fn from(response: Response<T>) -> Self {
        match response {
            Response::Reply(value) => Ok(value),
            Response::Trap { message, .. } => Err(Error::Remote(message)),
            Response::Done => Err(Error::EmptyResponse),
            Response::Fatal => Err(Error::EndOfStream),
        }
    }
}

impl<A, V: FromIterator<A>> FromIterator<Response<A>> for Response<V> {
    fn from_iter<T: IntoIterator<Item = Response<A>>>(iter: T) -> Self {
        let mut found_trap = None;
        let mut found_fatal = false;

        use Response::*;
        //No idea what I'm doing. This code has been inspired from https://github.com/rust-lang/rust/pull/59605
        let v: V = FromIterator::from_iter(iter.into_iter().filter_map(|elt| match elt {
            Done => None,
            Fatal => {
                found_fatal = true;
                None
            }
            Reply(value) => Some(value),

            trap @ Trap { .. } => {
                found_trap = Some(trap);
                None
            }
        }));

        match (found_trap, found_fatal) {
            (Some(Trap { message, category }), _) => Trap { category, message },

            (None, true) => Fatal,

            (None, false) => Reply(v),

            _ => unreachable!(),
        }
    }
}

/// Reply from `/system/resource/print` command
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SystemResources {
    pub uptime: String,
    pub version: String,
    pub build_time: String,
    #[serde(default)]
    pub factory_software: Option<String>,

    pub free_memory: u64,
    pub total_memory: u64,

    pub cpu: String,
    pub cpu_count: u8,
    #[serde(default)]
    pub cpu_load: u16,

    pub free_hdd_space: u64,
    pub total_hdd_space: u64,

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
#[derive(Debug, Clone, PartialEq, Eq)]
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

        /// IP address from which the connection originates (omitted for serial/local console)
        address: Option<String>,

        /// Mean of accessing admin interface: ssh, web, ...
        via: String,

        /// User group, as in which rights the user has on the system
        group: Option<String>,

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
        let address = raw.address;
        let via = raw.via.ok_or_else(|| de::Error::missing_field("via"))?;
        let group = raw.group;
        let radius = raw.radius.unwrap_or(false);

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

/// Event emitted by `/interface/listen` describing an interface state change or removal.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct InterfaceChange {
    /// Relative interface ID (e.g., `*1`)
    #[serde(rename = ".id")]
    pub id: String,

    /// True when the interface has been removed (`.dead=true`)
    #[serde(rename = ".dead", default)]
    pub is_dead: bool,

    /// Interface name
    #[serde(default)]
    pub name: Option<String>,

    /// Interface type (e.g., `ether`, `bridge`, `wg`)
    #[serde(rename = "type", default)]
    pub iface_type: Option<String>,

    /// Configured MTU (`auto` or numeric bytes)
    #[serde(default)]
    pub mtu: Option<InterfaceMTU>,

    /// Actual operating MTU in bytes
    #[serde(default)]
    pub actual_mtu: Option<u32>,

    /// Timestamp of last link-up event
    #[serde(alias = "last-link-up-time", default)]
    pub last_link_up: Option<String>,

    /// Number of link-down transitions
    #[serde(default)]
    pub link_downs: Option<u32>,

    /// Whether the interface is currently running
    #[serde(default)]
    pub running: Option<bool>,

    /// Whether the interface is enslaved to a bridge/bond
    #[serde(default)]
    pub slave: Option<bool>,

    /// Whether the interface is administratively disabled
    #[serde(default)]
    pub disabled: Option<bool>,

    /// Optional user comment
    #[serde(default)]
    pub comment: Option<String>,
}

/// Reply from `/system/interface/print` command
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Interface {
    #[serde(rename = ".id")]
    pub id: String,

    pub name: String,

    #[serde(rename = "type")]
    pub iface_type: String,

    #[serde(default)]
    pub mtu: Option<InterfaceMTU>,
    #[serde(default)]
    pub actual_mtu: Option<u32>,

    #[serde(alias = "last-link-up-time")]
    pub last_link_up: Option<String>,
    #[serde(default)]
    pub link_downs: Option<u32>,

    pub rx_byte: u64,
    pub tx_byte: u64,
    pub rx_packet: u64,
    pub tx_packet: u64,
    pub rx_drop: Option<u64>,
    pub tx_drop: Option<u64>,
    #[serde(default)]
    pub tx_queue_drop: Option<u64>,
    pub rx_error: Option<u64>,
    pub tx_error: Option<u64>,
    #[serde(default)]
    pub fp_rx_byte: Option<u64>,
    #[serde(default)]
    pub fp_tx_byte: Option<u64>,
    #[serde(default)]
    pub fp_rx_packet: Option<u64>,
    #[serde(default)]
    pub fp_tx_packet: Option<u64>,

    pub running: bool,
    #[serde(default)]
    pub slave: bool,

    pub disabled: bool,
}

/// Enum to represent the `mtu` field that can take either a number value or a text: 'auto'
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceMTU {
    /// 'auto' value
    Auto,

    /// MTU value in bytes
    Value(u32),
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

            other => other.parse::<u32>().map(InterfaceMTU::Value).map_err(|_| {
                de::Error::invalid_value(de::Unexpected::Str(other), &"'auto' or a valid number")
            }),
        }
    }
}
