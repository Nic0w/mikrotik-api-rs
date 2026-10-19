use std::collections::HashMap;

use futures::StreamExt;
use log::info;
use mikrotik_api::{Authenticated, MikrotikAPI, Response};

pub enum CommandType {
    OneOff,
    ArrayList,
    Streaming,
}

pub async fn custom_command(
    api: &mut MikrotikAPI<Authenticated>,
    cmd_type: CommandType,
    command: &str,
    proplist: Option<String>,
) {
    let mut attributes = Vec::new();

    if let Some(list) = proplist.as_deref() {
        attributes.push(("=.proplist", list))
    }

    let attributes = Some(attributes.as_slice());

    use CommandType::*;
    match cmd_type {
        OneOff => {
            match api
                .generic_oneshot_call::<HashMap<String, String>>(command, attributes)
                .await
            {
                Ok(map) => info!("Reply:\n{:#?}", map),
                Err(e) => eprintln!("Command failed: {}", e),
            }
        }
        ArrayList => {
            match api
                .generic_array_call::<HashMap<String, String>>(command, attributes)
                .await
            {
                Ok(map) => info!("Received {} replies:\n{:#?}", map.len(), map),
                Err(e) => eprintln!("Command failed: {}", e),
            }
        }

        Streaming => {
            let mut _tag = 0;
            let stream = api
                .generic_streaming_call::<HashMap<String, String>>(command, attributes, &mut _tag)
                .await;

            info!("Listening for events...");
            if let Err(e) = tokio::spawn(stream.for_each(move |item| async {
                match item {
                    Response::Reply(event) => info!("New event:\n{:#?}", event),
                    Response::Trap { message, .. } => eprintln!("Error event: {}", message),
                    Response::Fatal => eprintln!("Connection closed by router (Fatal)"),
                    Response::Done => info!("Stream finished"),
                }
            }))
            .await
            {
                eprintln!("Event listener task failed: {}", e);
            }
        }
    }
}
