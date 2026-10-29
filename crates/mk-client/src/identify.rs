use std::fmt::Display;

use human_bytes::human_bytes;
use mikrotik_api::{Authenticated, MikrotikAPI, SystemResources};
use serde::Deserialize;

struct PrettyResources(SystemResources);

impl Display for PrettyResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Uptime: {}", self.0.uptime)?;
        writeln!(f, "Version: {}", self.0.version)?;
        writeln!(f, "Build time: {}", self.0.build_time)?;
        writeln!(f, "Board: {}", self.0.board_name)?;
        writeln!(f, "Arch: {}", self.0.architecture_name)?;
        writeln!(
            f,
            "Memory (free/total): {} / {}",
            human_bytes(self.0.free_memory as f64),
            human_bytes(self.0.total_memory as f64)
        )?;
        write!(
            f,
            "HDD (free/total): {} / {}",
            human_bytes(self.0.free_hdd_space as f64),
            human_bytes(self.0.total_hdd_space as f64)
        )?;

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Identity {
    pub name: String,
}

pub async fn identify(api: &MikrotikAPI<Authenticated>, resources: bool) {
    let identity = match api
        .generic_oneshot_call::<Identity>("/system/identity/print", None)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Failed to fetch router identity: {}", e);
            return;
        }
    };

    println!("Name: '{}'", identity.name);

    if resources {
        match api.system_resources().await {
            Ok(res) => println!("{}", PrettyResources(res)),
            Err(e) => eprintln!("Failed to fetch system resources: {}", e),
        }
    }
}
