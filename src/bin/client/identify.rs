use std::fmt::Display;

use human_bytes::human_bytes;
use mikrotik_api::{Authenticated, MikrotikAPI, SystemResources};
use serde::Deserialize;

struct PrettyRessources(SystemResources);

impl Display for PrettyRessources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Uptime: {}", self.0.uptime)?;
        writeln!(f, "Version: {}", self.0.version)?;
        writeln!(f, "Build time: {}", self.0.build_time)?;
        writeln!(f, "Board: {}", self.0.board_name)?;
        writeln!(f, "Arch: {}", self.0.architecture_name)?;
        writeln!(
            f,
            "Memory (free/total): {} / {}",
            human_bytes(self.0.free_memory),
            human_bytes(self.0.total_memory)
        )?;
        write!(
            f,
            "HDD (free/total): {} / {}",
            human_bytes(self.0.free_hdd_space),
            human_bytes(self.0.total_hdd_space)
        )?;

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Identity {
    pub name: String,
}

pub async fn identify(api: &mut MikrotikAPI<Authenticated>, ressources: bool) {
    let identity = api
        .generic_oneshot_call::<Identity>("/system/identity/print", None)
        .await
        .unwrap();

    println!("Name: '{}'", identity.name);

    if ressources {
        let ressources = api.system_resources().await.unwrap();
        println!("{}", PrettyRessources(ressources))
    }
}
