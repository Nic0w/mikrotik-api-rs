use std::collections::HashMap;

use futures::StreamExt;
use log::info;
use mikrotik_api::{MikrotikAPI, Authenticated, Response};


pub enum CommandType {
    OneOff,
    ArrayList,
    Streaming
}

pub async fn custom_command(api: &mut MikrotikAPI<Authenticated>, command: &str, cmd_type: CommandType) {

    use CommandType::*;
    match cmd_type {

        OneOff => {
            let map = api.generic_oneshot_call::<HashMap<String, String>>(command, None)
                .await
                .unwrap();

            info!("Reply:\n{:#?}", map)
        },
        ArrayList => {
            let map = api.generic_array_call::<HashMap<String, String>>(command, None)
                .await
                .unwrap();

            info!("Received {} replies:\n{:#?}", map.len(), map)
        },

        Streaming => {

            let mut _tag = 0;
            let stream = api.generic_streaming_call::<HashMap<String, String>>(command, None, &mut _tag).await;

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