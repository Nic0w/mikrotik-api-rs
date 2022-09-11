#![deny(missing_docs)]

use std::io::{self};

use tokio::net::{TcpStream, ToSocketAddrs};

mod api;

pub use api::model::{
    ActiveUser, Interface, InterfaceChange, InterfaceMTU, Response, SystemResources,
};
pub use api::{Authenticated, Disconnected, MikrotikAPI};

pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<MikrotikAPI<Disconnected>> {
    let socket = TcpStream::connect(addr).await?;

    Ok(MikrotikAPI::new(socket))
}