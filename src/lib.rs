//! # Library and minimal client for RouterOS' proprietary API
//!
//! Mikrotik RouterOS exposes on port TCP/8728 a semi-textual API composed of words and sentences.
//! This API allow power-users to request data about the router state, set configuration, listen to some events, ... using a syntax similar to the one used in the CLI.
//!
//! For a more in-depth description of the API, please see the [official Mikrotik wiki](https://wiki.mikrotik.com/wiki/Manual:API).  
//!
//! ## The library

//! Based on tokio and fully asynchronous, the library allows to deal with all the API's uses cases :
//! - simple requests with one-off answers, like `/system/identity/print`,
//! - simple requests with array-like answers, like `/interface/print`,
//! - simple requests to listen to events, like `/user/active/listen`,
//! - cancel streaming commands with `/cancel`
//!
//! ### Usage
//!
//! Plaintext connections to the API service (default port 8728) are established with `connect`:
//! ```rust,no_run
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! let disconnected = mikrotik_api::connect("192.168.88.1:8728").await?;
//! let mut api = disconnected.authenticate("admin", "password").await?;
//! # Ok(())
//! # }
//! ```
//!
//! Encrypted connections to RouterOS API-SSL (default port 8729) are established with `connect_tls`:
//! ```rust,no_run
//! # #[cfg(feature = "tls")]
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! let disconnected = mikrotik_api::connect_tls("192.168.88.1:8729", "192.168.88.1").await?;
//! let mut api = disconnected.authenticate("admin", "password").await?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "tls"))]
//! # fn doc() {}
//! ```
//!
//! Custom streams or arbitrary async transports (e.g. mock duplex streams, SSH tunnels) can be wrapped with `MikrotikAPI::from_stream` or `MikrotikAPI::from_parts`.
//!
//! Eight functions are then available:
//! - `system_resources` will make a call to `/system/resource/print`
//! - `interfaces` will retrieve a list of interfaces with all their properties (``/interface/print``)
//! - `active_users` returns a `Stream` of events regarding user activity (login & logout)
//! - `interfaces_changes` (or `interface_changes`) returns a `Stream` of events regarding changes to interfaces (up, down, ...)
//! - `cancel` cancels a streaming command given its tag
//! - `generic_oneshot_call` allows to call any endpoint providing a one-off answer. Thanks to type inference, answer is returned in the user's object of choice. Example:

//! ```rust,no_run
//! # use serde::Deserialize;
//! # use mikrotik_api::MikrotikAPI;
//! # async fn doc(mut api: MikrotikAPI<mikrotik_api::Authenticated>) -> Result<(), Box<dyn std::error::Error>> {
//! #[derive(Debug, Deserialize)]
//! struct Identity {
//!   pub name: String,
//! }

//! let identity = api
//!   .generic_oneshot_call::<Identity>("/system/identity/print", None)
//!   .await
//!   .unwrap();

//! println!("Name: '{}'", identity.name);
//! # Ok(())
//! # }
//! ```

//! - `generic_array_call` will do the same job but for endpoints providing multiples (but finite) answers
//! - `generic_streaming_call` will provide a `Stream` of `Response` for any endpoint supporting the `listen` command. Example:
//! ```rust,no_run
//! # use serde::Deserialize;
//! # use futures::StreamExt;
//! # use mikrotik_api::{MikrotikAPI, Response};
//! # async fn doc(mut api: MikrotikAPI<mikrotik_api::Authenticated>) -> Result<(), Box<dyn std::error::Error>> {
//! #[derive(Debug, Deserialize)]
//! struct Interface {
//!   pub name: String,
//!
//!   #[serde(default)]
//!   pub running: bool
//! }
//!
//! let mut tag: u16 = 0;
//!
//! let changes = api
//!   .generic_streaming_call::<Interface>("/interface/listen", None, &mut tag)
//!   .await; // `tag` allows us to cancel the stream later on.
//!
//! tokio::spawn(changes.for_each(|item| async move {
//!
//!   if let Response::Reply(iface) = item {
//!
//!       let up_down = if iface.running { "up" } else { "down" };
//!
//!       println!("Interface {} is {}", iface.name, up_down);
//!   }
//!
//! })).await;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
use std::io;

use tokio::net::{TcpStream, ToSocketAddrs};

mod api;

pub use api::model::{
    ActiveUser, Interface, InterfaceChange, InterfaceMTU, Response, SystemResources, TrapCategory,
};
#[cfg(feature = "tls")]
pub use api::tls::{TlsConfig, TlsConfigBuilder};
pub use api::{Authenticated, DeserializerError, Disconnected, Error, MikrotikAPI};

/// Given an address, opens a plaintext connection to the remote API service (typically port 8728).
/// The returned object is in a Disconnected state.
pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<MikrotikAPI<Disconnected>> {
    let socket = TcpStream::connect(addr).await?;

    Ok(MikrotikAPI::new(socket))
}

/// Given an address and server domain name, opens an encrypted TLS connection to the remote API-SSL service (typically port 8729).
/// Uses default TLS configuration.
/// The returned object is in a Disconnected state.
#[cfg(feature = "tls")]
pub async fn connect_tls<A: ToSocketAddrs>(
    addr: A,
    domain: &str,
) -> Result<MikrotikAPI<Disconnected>, Error> {
    connect_tls_with_config(addr, domain, TlsConfig::default()).await
}

/// Given an address, server domain name, and [`TlsConfig`], opens an encrypted TLS connection to the remote API-SSL service (typically port 8729).
/// The returned object is in a Disconnected state.
#[cfg(feature = "tls")]
pub async fn connect_tls_with_config<A: ToSocketAddrs>(
    addr: A,
    domain: &str,
    config: TlsConfig,
) -> Result<MikrotikAPI<Disconnected>, Error> {
    let tls_stream = api::tls::connect_tls_stream(addr, domain, &config).await?;

    Ok(MikrotikAPI::from_stream(tls_stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_reexports_accessible() {
        // Verify Error and its variants are publicly accessible and usable
        let err = Error::Remote("permission denied".to_string());
        match &err {
            Error::Remote(msg) => assert_eq!(msg, "permission denied"),
            _ => panic!("unexpected error variant"),
        }
        assert_eq!(format!("{}", err), "error from router: permission denied");

        // Verify DeserializerError is publicly accessible and usable
        let de_err = DeserializerError::MissingWord;
        match &de_err {
            DeserializerError::MissingWord => {}
            _ => panic!("unexpected deserializer error variant"),
        }

        // Verify Error::Deserialize and From<DeserializerError> for Error
        let err_de: Error = de_err.clone().into();
        match &err_de {
            Error::Deserialize(DeserializerError::MissingWord) => {}
            _ => panic!("unexpected Error::Deserialize variant"),
        }
        assert_eq!(
            format!("{}", err_de),
            "deserialization error: missing word to finish the sentence"
        );

        // Verify TrapCategory is publicly accessible and usable
        let category = TrapCategory::ArgumentValueFailure;
        assert_eq!(category as u8, 1);
        let resp: Response<()> = Response::Trap {
            category: Some(TrapCategory::MissingItemOrCommand),
            message: "no such item".to_string(),
        };
        match resp {
            Response::Trap {
                category: Some(TrapCategory::MissingItemOrCommand),
                message,
            } => {
                assert_eq!(message, "no such item");
            }
            _ => panic!("unexpected response variant"),
        }

        // Verify Error::Tls is publicly accessible and formats correctly
        let tls_err = Error::Tls("handshake failed".to_string());
        match &tls_err {
            Error::Tls(msg) => assert_eq!(msg, "handshake failed"),
            _ => panic!("unexpected error variant"),
        }
        assert_eq!(format!("{}", tls_err), "tls error: handshake failed");

        #[cfg(feature = "tls")]
        {
            let config = TlsConfig::builder()
                .danger_accept_invalid_certs(true)
                .build();
            assert!(config.danger_accept_invalid_certs);
        }
    }
}
