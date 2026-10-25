# Library and minimal client for RouterOS' proprietary API

Mikrotik RouterOS exposes on port TCP/8728 a semi-textual API composed of words and sentences. 
This API allow power-users to request data about the router state, set configuration, listen to some events, ... using a syntax similar to the one used in the CLI.

For a more in-depth description of the API, please see the [official Mikrotik wiki](https://wiki.mikrotik.com/wiki/Manual:API).  

While it may look simple to implement, there is some complexity involved in dealing with all the use cases. For example, one can send multiple 'listen' commands that will stream events as they come, while requesting more simple data composed of a one-off response. 
In essence, the API is highly asynchronous and multiplexes responses to requests in between streams of events; particularities that make the Rust ecosystem especially suited for such work !

## The library

Based on tokio and fully asynchronous, the library allows to deal with all the API's uses cases :
 - simple requests with one-off answers, like `/system/identity/print`,
 - simple requests with array-like answers, like `/interface/print`,
 - simple requests to listen to events, like `/user/active/listen`,
 - cancel streaming commands with `/cancel`

There is still much to do, like support query words, ...

### Usage

#### Plaintext Connection (Port 8728)
```rust
let disconnected = mikrotik_api::connect("192.168.88.1:8728").await?;
let mut api = disconnected.authenticate("admin", "password").await?;
```

#### Encrypted TLS Connection (Port 8729)
```rust
// Connect using default TLS configuration (system roots + webpki-roots)
let disconnected = mikrotik_api::connect_tls("192.168.88.1:8729", "192.168.88.1").await?;
let mut api = disconnected.authenticate("admin", "password").await?;

// Or configure custom CA / allow self-signed certificates:
let config = mikrotik_api::TlsConfig::builder()
    .danger_accept_invalid_certs(true) // For self-signed certificates
    .build();
let disconnected = mikrotik_api::connect_tls_with_config("192.168.88.1:8729", "192.168.88.1", config).await?;
```

#### Custom Streams
Any duplex async stream (e.g. Unix sockets, SSH tunnels, mock streams) can be wrapped directly:
```rust
let api = mikrotik_api::MikrotikAPI::from_stream(my_duplex_stream);
```

Eight functions are then available:
 - `system_resources` will make a call to `/system/resource/print`
 - `interfaces` will retrieve a list of interfaces with all their properties (``/interface/print``)
 - `active_users` returns a `Stream` of events regarding user activity (login & logout)
 - `interfaces_changes` (or `interface_changes`) returns a `Stream` of events regarding changes to interfaces (up, down, ...)
 - `cancel` cancels a streaming command given its tag
 - `generic_oneshot_call` allows to call any endpoint providing a one-off answer. Thanks to type inference, answer is returned in the user's object of choice. Example:

```rust
#[derive(Debug, Deserialize)]
struct Identity {
    pub name: String,
}

let identity = api
    .generic_oneshot_call::<Identity>("/system/identity/print", None)
    .await
    .unwrap();

println!("Name: '{}'", identity.name);
```

 - `generic_array_call` will do the same job but for endpoints providing multiples (but finite) answers
 - `generic_streaming_call` will provide a `Stream` of `Response` for any endpoint supporting the `listen` command. Example:
 ```rust
#[derive(Debug, Deserialize)]
struct Interface {
    pub name: String,

    #[serde(default)]
    pub running: bool,
}

let mut tag: u16 = 0;

let changes = api
    .generic_streaming_call::<Interface>("/interface/listen", None, &mut tag)
    .await; // `tag` allows us to cancel the stream later on.

tokio::spawn(changes.for_each(|item| async move {

    if let Response::Reply(iface) = item {

        let up_down = if iface.running { "up" } else { "down" };

        println!("Interface {} is {}", iface.name, up_down);
    }

})).await;
 ```

 ## The client

 As of now it serves more as an example of library usage rather than having a real, purposeful goal.
 
 ```
mk-client 0.4.0

USAGE:
    mk-client [OPTIONS] --address <ADDRESS> --login <LOGIN> <SUBCOMMAND>

OPTIONS:
    -A, --address <ADDRESS>        <HOST> or <HOST>:<PORT>
    -s, --ssl                      Connect using SSL/TLS (default port: 8729)
    -k, --insecure                 Allow insecure / self-signed server certificates
        --tls-domain <TLS_DOMAIN>  Override TLS server name for SNI and certificate validation
        --tls-ca <TLS_CA>          Path to custom CA certificate file (PEM format)
    -L, --login <LOGIN>            
    -P, --password <PASSWORD>      
    -h, --help                     Print help
    -V, --version                  Print version

SUBCOMMANDS:
    active-users    
    help            Print this message or the help of the given subcommand(s)
    identify        
 ```

Right now it comes with three subcommands:
 - `identify`: will print your router's name and its resources (with `--full`)
 - `active-users`: will listen to user activity and display events in a log-like manner
 - `custom`, the best of all, allows to call arbitrary commands of all sorts (one-off, arraylist, streaming). Example, listening in real-time to log events over SSL with a self-signed certificate:
  
```bash
$ target/release/mk-client -A 192.168.88.1 --ssl -k -L admin -P "P@ssw0rd" custom --listen "/log/listen"
2022-09-07T21:38:04.659Z INFO [client::custom] New event:
{
    ".id": "*1CB36",
    "time": "21:38:04",
    "message": "user admin logged in from 192.168.88.10 via web",
    "topics": "system,info,account",
}
2022-09-07T21:38:05.559Z INFO [client::custom] New event:
{
    "topics": "firewall,info",
    ".id": "*1CB37",
    "time": "21:38:05",
    "message": "BLOCKED input: in:wan(vlan42) out:(unknown 0), src-mac aa:bb:cc:dd:ee:ff, proto TCP (SYN), 1.2.3.4:42845->4.3.2.1:24032, len 40",
}

```

 # DISCLAIMER

 This software is provided as-is, without any warranty. I am not in any way affiliated with Mikrotik and I am not responsible of any damage that you may cause to your router while using this software.



