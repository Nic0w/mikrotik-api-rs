use clap::{CommandFactory, Parser};
use futures::StreamExt;
use log::info;

use mikrotik_api::{self, Response};

use crate::{config::Args, custom::CommandType};

mod config;
mod custom;
mod identify;

#[tokio::main]
pub async fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();

    let args = Args::parse();

    let api = mikrotik_api::connect(args.address).await.unwrap();

    let mut api = match api.authenticate(&args.login, &args.password).await {
        Ok(api) => api,

        Err(e) => {
            println!("{:?}", e);

            return;
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
                        clap::ErrorKind::ArgumentConflict,
                        "Arguments are mutualy exculisve",
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

            tokio::spawn(stream.for_each(move |item| async {
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
            .unwrap();
        }
    };
}
