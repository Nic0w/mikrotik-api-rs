//! # Library and minimal client for RouterOS' proprietary API
//! 
//! Mikrotik RouterOS exposes on port TCP/8278 a semi-textual API composed of words and sentences. 
//! This API allow power-users to request data about the router state, set configuration, listen to some events, ... using a syntax similar to the one used in the CLI.
//!
//! For a more in-depth description of the API, please see the [official Mikrotik wiki](https://wiki.mikrotik.com/wiki/Manual:API).  
//!
//! ## The library

//! Based on tokio and fully asynchronous, the library allows to deal with all the API's uses cases :
//! - simple requests with one-off answers, like `/system/identity/print`,
//! - simple requests with array-like answers, like `/interfaces/print`,
//! - simple requests to listen to events, like `/user/active/listen`,
//! - cancel streaming commands with `/cancel`
//!
//! ### Usage
//!
//! The library exposes only one function: `connect`, that makes a TCP connection to the provided address.
//! If successful, a `MikrotikAPI<Disconnected>` object is returned.
//! It is then necessary to `authenticate` to get a `MikrotikAPI<Authenticated>` object.

//! Eight functions are then available:
//! - `system_resources` will make a call to `/system/resource/print`
//! - `interfaces` will retrieve a list of interfaces with all their properties (``/interfaces/print``)
//! - `active_users` returns a `Stream` of events regarding user activity (login & logout)
//! - `interface_changes` returns a `Stream` of events regarding changes to interfaces (up, down, ...)
//! - `cancel` cancels a streaming command given its tag
//! - `generic_oneshot_call` allows to call any endpoint providing a one-off answer. Thanks to type inference, answer is returned in the user's object of choice. Example:

//! ```rust
//! #[derive(Debug, Deserialize)]
//! struct Identity {
//!   pub name: String,
//! }

//! let identity = api
//!   .generic_oneshot_call::<Identity>("/system/identity/print", None)
//!   .await
//!   .unwrap();

//! println!("Name: '{}'", identity.name);
//! ```

//! - `generic_array_call` will do the same job but for endpoints providing multiples (but finite) answers
//! - `generic_streaming_call` will provide a `Stream` of `Response` for any endpoint supporting the `listen` command. Example:
//! ```rust
//! #[derive(Debug, Deserialize)]
//! struct Interface {
//!   pub name: String,
//!
//!   #[serde(default)]
//!   pub running: bool
//!}
//!
//! let mut tag: u16 = 0;
//!
//! let changes = api
//!   .generic_streaming_call::<Interface>("/interface/listen", None, &mut tag); //`tag` allows us to cancel the stream later on.
//!
//! tokio::spawn(changes.for_each(|item| async move {
//!
//!   if let Reponse::Reply(iface) = item {
//!
//!       let up_down = if iface.running { "up" } else { "down" };
//!
//!       println!("Interface {} is {}", iface.name, up_down);
//!   }
//!
//! })).await;
//! ```

#![deny(missing_docs)]
use std::io;

use tokio::net::{TcpStream, ToSocketAddrs};

mod api;

pub use api::model::{
    ActiveUser, Interface, InterfaceChange, InterfaceMTU, Response, SystemResources,
};
pub use api::{Authenticated, Disconnected, MikrotikAPI};

/// Given an address, opens a connection to the remote API service
/// the returned object is in a Disconnected state
pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<MikrotikAPI<Disconnected>> {
    let socket = TcpStream::connect(addr).await?;

    Ok(MikrotikAPI::new(socket))
}