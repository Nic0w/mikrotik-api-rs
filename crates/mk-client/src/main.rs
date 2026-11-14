use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use clap::{CommandFactory, Parser};
use dialoguer::Password;
use futures::StreamExt;
use log::info;

use mikrotik_api::{self, Response};

use crate::{config::Args, custom::CommandType};

mod config;
mod custom;
mod identify;

#[tokio::main]
pub async fn main() {
    let _ = simple_logger::init_with_level(log::Level::Info);

    let args = Args::parse();

    let (target_addr, default_domain) = config::resolve_target(&args.address, args.ssl);
    let _ = &default_domain;

    let api = if args.ssl {
        #[cfg(feature = "tls")]
        {
            let domain = args.tls_domain.unwrap_or(default_domain);
            let mut builder =
                mikrotik_api::TlsConfig::builder().danger_accept_invalid_certs(args.insecure);

            if let Some(ca_path) = &args.tls_ca {
                builder = match builder.add_ca_cert_file(ca_path) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Failed to read CA certificate from {:?}: {}", ca_path, e);
                        std::process::exit(1);
                    }
                };
            }

            let config = builder.build();

            match mikrotik_api::connect_tls_with_config(&target_addr, &domain, config).await {
                Ok(api) => api,
                Err(e) => {
                    eprintln!(
                        "Failed to connect via TLS to {} (domain: {}): {}",
                        target_addr, domain, e
                    );
                    std::process::exit(1);
                }
            }
        }
        #[cfg(not(feature = "tls"))]
        {
            eprintln!(
                "Error: SSL/TLS support was disabled at compile time (feature 'tls' is not enabled)"
            );
            std::process::exit(1);
        }
    } else {
        match mikrotik_api::connect(&target_addr).await {
            Ok(api) => api,
            Err(e) => {
                eprintln!("Failed to connect to {}: {}", target_addr, e);
                std::process::exit(1);
            }
        }
    };

    let password = match args.password {
        Some(p) => p,
        None => match Password::new().with_prompt("Password").interact() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to read password: {}", e);
                std::process::exit(1);
            }
        },
    };

    let api = match api.authenticate(&args.login, &password).await {
        Ok(api) => api,
        Err(e) => {
            eprintln!("Authentication failed: {}", e);
            std::process::exit(1);
        }
    };

    use config::Command::*;
    match args.command {
        Identify { full } => {
            if identify::identify(&api, full).await.is_err() {
                std::process::exit(1);
            }
        }

        Custom {
            one_off,
            array_list,
            listen,
            proplist,
            attributes,
            command,
        } => {
            let cmd_type = match (one_off, array_list, listen) {
                (true, false, false) => CommandType::OneOff,

                (false, true, false) => CommandType::ArrayList,

                (false, false, true) => CommandType::Streaming,

                (false, false, false) => {
                    let mut cmd = Args::command();
                    cmd.error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "One of --one-off, --array-list, or --listen is required",
                    )
                    .exit();
                }

                _ => {
                    let mut cmd = Args::command();
                    cmd.error(
                        clap::error::ErrorKind::ArgumentConflict,
                        "Arguments --one-off, --array-list, and --listen are mutually exclusive",
                    )
                    .exit();
                }
            };

            if custom::custom_command(&api, cmd_type, &command, proplist, &attributes)
                .await
                .is_err()
            {
                std::process::exit(1);
            }
        }

        ActiveUsers => {
            let mut tag = 0;

            let stream = api.active_users(&mut tag).await;

            info!("Listening for active users...");

            let failed = Arc::new(AtomicBool::new(false));
            let task_failed = Arc::clone(&failed);

            if let Err(e) = tokio::spawn(stream.for_each(move |item| {
                let task_failed = Arc::clone(&task_failed);
                async move {
                    match item {
                        Response::Reply(user) => {
                            use mikrotik_api::ActiveUser::*;
                            match user {
                                Dead(id) => info!("User id {} disconnected", id),
                                Active {
                                    id,
                                    name,
                                    address,
                                    via,
                                    ..
                                } => {
                                    info!(
                                        "User '{}' (id: {}) logged in via {} from {}",
                                        name,
                                        id,
                                        via,
                                        address.as_deref().unwrap_or("unknown")
                                    );
                                }
                            }
                        }
                        Response::Trap { message, .. } => {
                            eprintln!("Active users stream error: {}", message);
                            task_failed.store(true, Ordering::Relaxed);
                        }
                        Response::Fatal => {
                            eprintln!("Connection closed by router (Fatal)");
                            task_failed.store(true, Ordering::Relaxed);
                        }
                        Response::Done => info!("Active users stream finished"),
                    }
                }
            }))
            .await
            {
                eprintln!("Active users task failed: {}", e);
                std::process::exit(1);
            }

            if failed.load(Ordering::Relaxed) {
                std::process::exit(1);
            }
        }
    }
}
