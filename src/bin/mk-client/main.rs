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

    let api = match mikrotik_api::connect(&args.address).await {
        Ok(api) => api,
        Err(e) => {
            eprintln!("Failed to connect to {}: {}", args.address, e);
            std::process::exit(1);
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

    let mut api = match api.authenticate(&args.login, &password).await {
        Ok(api) => api,
        Err(e) => {
            eprintln!("Authentication failed: {}", e);
            std::process::exit(1);
        }
    };

    use config::Command::*;
    match args.command {
        Identify { full } => identify::identify(&mut api, full).await,

        Custom {
            one_off,
            array_list,
            listen,
            proplist,
            command,
        } => {
            let cmd_type = match (one_off, array_list, listen) {
                (true, false, false) => CommandType::OneOff,

                (false, true, false) => CommandType::ArrayList,

                (false, false, true) => CommandType::Streaming,

                _ => {
                    let mut cmd = Args::command();
                    cmd.error(
                        clap::error::ErrorKind::ArgumentConflict,
                        "Arguments are mutually exclusive",
                    )
                    .exit();
                }
            };

            custom::custom_command(&mut api, cmd_type, &command, proplist).await;
        }

        ActiveUsers => {
            let mut tag = 0;

            let stream = api.active_users(&mut tag).await;

            info!("Listening for active users...");

            if let Err(e) = tokio::spawn(stream.for_each(move |item| async {
                if let Response::Reply(user) = item {
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
                                name, id, via, address
                            );
                        }
                    }
                }
            }))
            .await
            {
                eprintln!("Active users task failed: {}", e);
            }
        }
    };
}
