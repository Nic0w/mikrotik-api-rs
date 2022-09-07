use std::collections::HashMap;

use mikrotik_api::{MikrotikAPI, Authenticated};


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

            println!("{:#?}", map)
        },
        ArrayList => {
            let map = api.generic_array_call::<HashMap<String, String>>(command, None)
            .await
            .unwrap();

        println!("{:#?}", map)
        },

        Streaming => todo!(),
    }

}