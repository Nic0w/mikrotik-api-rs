use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use futures::StreamExt;
use log::info;
use mikrotik_api::{Authenticated, MikrotikAPI, Response};

pub enum CommandType {
    OneOff,
    ArrayList,
    Streaming,
}

pub async fn custom_command(
    api: &MikrotikAPI<Authenticated>,
    cmd_type: CommandType,
    command: &str,
    proplist: Option<String>,
) -> Result<(), ()> {
    let mut attributes = Vec::new();

    if let Some(list) = proplist.as_deref() {
        attributes.push((".proplist", list))
    }

    let attributes = Some(attributes.as_slice());

    use CommandType::*;
    match cmd_type {
        OneOff => {
            match api
                .generic_oneshot_call::<HashMap<String, String>>(command, attributes)
                .await
            {
                Ok(map) => {
                    info!("Reply:\n{:#?}", map);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Command failed: {}", e);
                    Err(())
                }
            }
        }
        ArrayList => {
            match api
                .generic_array_call::<HashMap<String, String>>(command, attributes)
                .await
            {
                Ok(map) => {
                    info!("Received {} replies:\n{:#?}", map.len(), map);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Command failed: {}", e);
                    Err(())
                }
            }
        }

        Streaming => {
            let mut _tag = 0;
            let stream = api
                .generic_streaming_call::<HashMap<String, String>>(command, attributes, &mut _tag)
                .await;

            info!("Listening for events...");
            let failed = Arc::new(AtomicBool::new(false));
            let task_failed = Arc::clone(&failed);

            if let Err(e) = tokio::spawn(stream.for_each(move |item| {
                let task_failed = Arc::clone(&task_failed);
                async move {
                    match item {
                        Response::Reply(event) => info!("New event:\n{:#?}", event),
                        Response::Trap { message, .. } => {
                            eprintln!("Error event: {}", message);
                            task_failed.store(true, Ordering::Relaxed);
                        }
                        Response::Fatal => {
                            eprintln!("Connection closed by router (Fatal)");
                            task_failed.store(true, Ordering::Relaxed);
                        }
                        Response::Done => info!("Stream finished"),
                    }
                }
            }))
            .await
            {
                eprintln!("Event listener task failed: {}", e);
                return Err(());
            }

            if failed.load(Ordering::Relaxed) {
                Err(())
            } else {
                Ok(())
            }
        }
    }
}
