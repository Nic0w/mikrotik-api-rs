use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short = 'A', long, help = "<HOST> or <HOST>:<PORT>")]
    pub address: String,

    #[clap(short = 's', long, help = "Connect using SSL/TLS (default port: 8729)")]
    pub ssl: bool,

    #[clap(
        short = 'k',
        long,
        help = "Allow insecure / self-signed server certificates"
    )]
    pub insecure: bool,

    #[clap(
        long,
        help = "Override TLS server name for SNI and certificate validation"
    )]
    pub tls_domain: Option<String>,

    #[clap(long, help = "Path to custom CA certificate file (PEM format)")]
    pub tls_ca: Option<PathBuf>,

    #[clap(short = 'L', long)]
    pub login: String,

    #[clap(short = 'P', long)]
    pub password: Option<String>,

    #[clap(subcommand)]
    pub command: Command,
}

/// Resolves the socket address and default domain name from the user input.
///
/// If no port is specified, defaults to 8729 for SSL/TLS, or 8728 for plaintext.
pub fn resolve_target(address: &str, ssl: bool) -> (String, String) {
    let default_port = if ssl { 8729 } else { 8728 };
    let addr = address.trim();

    // Check if bracketed IPv6: e.g. [::1] or [::1]:8729
    if addr.starts_with('[') {
        if let Some(bracket_end) = addr.find(']') {
            let host = &addr[1..bracket_end];
            let rest = &addr[bracket_end + 1..];
            if let Some(port_str) = rest.strip_prefix(':') {
                if let Ok(port) = port_str.parse::<u16>() {
                    return (format!("[{}]:{}", host, port), host.to_string());
                }
            }
            return (format!("[{}]:{}", host, default_port), host.to_string());
        }
    }

    // Check for IPv4 or hostname with port: e.g. host:port
    if let Some((host, port_str)) = addr.rsplit_once(':') {
        // Verify it's a port number and not an unbracketed IPv6 address (like ::1)
        if let Ok(port) = port_str.parse::<u16>() {
            if !host.contains(':') {
                return (format!("{}:{}", host, port), host.to_string());
            }
        }
    }

    // If it contains colons and wasn't bracketed, treat whole string as IPv6
    if addr.contains(':') {
        return (format!("[{}]:{}", addr, default_port), addr.to_string());
    }

    // Standard hostname or IPv4 without port
    (format!("{}:{}", addr, default_port), addr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_target_defaults_and_ports() {
        // Plaintext default port 8728
        assert_eq!(
            resolve_target("192.168.88.1", false),
            ("192.168.88.1:8728".to_string(), "192.168.88.1".to_string())
        );

        // SSL default port 8729
        assert_eq!(
            resolve_target("192.168.88.1", true),
            ("192.168.88.1:8729".to_string(), "192.168.88.1".to_string())
        );

        // Explicit port preserved
        assert_eq!(
            resolve_target("192.168.88.1:18728", false),
            ("192.168.88.1:18728".to_string(), "192.168.88.1".to_string())
        );
        assert_eq!(
            resolve_target("192.168.88.1:18729", true),
            ("192.168.88.1:18729".to_string(), "192.168.88.1".to_string())
        );

        // Hostnames
        assert_eq!(
            resolve_target("router.lan", true),
            ("router.lan:8729".to_string(), "router.lan".to_string())
        );
        assert_eq!(
            resolve_target("router.lan:9999", true),
            ("router.lan:9999".to_string(), "router.lan".to_string())
        );

        // IPv6
        assert_eq!(
            resolve_target("[::1]", true),
            ("[::1]:8729".to_string(), "::1".to_string())
        );
        assert_eq!(
            resolve_target("[::1]:9999", false),
            ("[::1]:9999".to_string(), "::1".to_string())
        );
        assert_eq!(
            resolve_target("::1", true),
            ("[::1]:8729".to_string(), "::1".to_string())
        );
    }

    #[test]
    fn test_parse_key_val() {
        assert_eq!(
            parse_key_val("request=syntax").unwrap(),
            ("request".to_string(), "syntax".to_string())
        );
        assert_eq!(
            parse_key_val("?comment=a=b=c").unwrap(),
            ("?comment".to_string(), "a=b=c".to_string())
        );
        assert_eq!(
            parse_key_val("empty=").unwrap(),
            ("empty".to_string(), String::new())
        );
        assert!(parse_key_val("no_equals").is_err());
        assert!(parse_key_val("=missing_key").is_err());
    }

    #[test]
    fn test_custom_command_repeatable_attributes() {
        let args = Args::try_parse_from([
            "mk-client",
            "-A",
            "192.168.88.1",
            "-L",
            "admin",
            "-P",
            "secret",
            "custom",
            "--array-list",
            "-p",
            "type,name",
            "-a",
            "request=child",
            "--attribute",
            "path=ip,address",
            "/console/inspect",
        ])
        .unwrap();

        match args.command {
            Command::Custom {
                array_list,
                proplist,
                attributes,
                command,
                ..
            } => {
                assert!(array_list);
                assert_eq!(proplist.as_deref(), Some("type,name"));
                assert_eq!(
                    attributes,
                    vec![
                        ("request".to_string(), "child".to_string()),
                        ("path".to_string(), "ip,address".to_string()),
                    ]
                );
                assert_eq!(command, "/console/inspect");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    Identify {
        #[clap(long, help = "show target's resources as well.")]
        full: bool,
    },

    ActiveUsers,

    Custom {
        #[clap(long, help = "run one-off command")]
        one_off: bool,

        #[clap(long, help = "run array-list command")]
        array_list: bool,

        #[clap(long, help = "run listen command")]
        listen: bool,

        #[clap(short, long, help = "set .proplist")]
        proplist: Option<String>,

        #[clap(
            short = 'a',
            long = "attribute",
            value_parser = parse_key_val,
            value_name = "KEY=VALUE",
            help = "pass command attribute (repeatable, e.g. -a request=syntax -a path=ip,address)"
        )]
        attributes: Vec<(String, String)>,

        command: String,
    },
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let (key, val) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;

    if key.is_empty() {
        return Err(format!("invalid KEY=VALUE: empty key in `{s}`"));
    }

    Ok((key.to_string(), val.to_string()))
}
