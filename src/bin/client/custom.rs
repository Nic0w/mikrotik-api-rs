use std::collections::HashMap;

use futures::StreamExt;
use log::info;
use mikrotik_api::{MikrotikAPI, Authenticated, Response};


pub enum CommandType {
    OneOff,
    ArrayList,
    Streaming
}

pub async fn custom_command(api: &mut MikrotikAPI<Authenticated>, cmd_type: CommandType, command: &str, proplist: Option<String>) {

    let mut attributes = Vec::new();

    if let Some(list) = proplist.as_deref() {
        attributes.push(("=.proplist", list))
    }

    let attributes = Some(attributes.as_slice());

    use CommandType::*;
    match cmd_type {

        OneOff => {
            let map = api.generic_oneshot_call::<HashMap<String, String>>(command, attributes)
                .await
                .unwrap();

            info!("Reply:\n{:#?}", map)
        },
        ArrayList => {
            let map = api.generic_array_call::<HashMap<String, String>>(command, attributes)
                .await
                .unwrap();

            info!("Received {} replies:\n{:#?}", map.len(), map)
        },

        Streaming => {

            let mut _tag = 0;
            let stream = api.generic_streaming_call::<HashMap<String, String>>(command, attributes, &mut _tag).await;

            tokio::spawn(stream.for_each(move |item| async {
                if let Response::Reply(event) = item {

                    info!("New event:\n{:#?}", event)
                }
            }))
            .await
            .unwrap();
        },
    }

}