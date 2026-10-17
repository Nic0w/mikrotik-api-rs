use std::{env, time::Duration};

use mikrotik_api::{Authenticated, MikrotikAPI};
use serde::Deserialize;
use tokio::time::timeout;

fn router_config() -> (String, String, String) {
    let addr = env::var("MIKROTIK_ADDR")
        .expect("MIKROTIK_ADDR environment variable must be set for functional tests");
    let user = env::var("MIKROTIK_USER")
        .expect("MIKROTIK_USER environment variable must be set for functional tests");
    let pass = env::var("MIKROTIK_PASS")
        .expect("MIKROTIK_PASS environment variable must be set for functional tests");

    (addr, user, pass)
}

async fn connect_and_login() -> MikrotikAPI<Authenticated> {
    let (addr, user, pass) = router_config();

    let disconnected = timeout(Duration::from_secs(5), mikrotik_api::connect(&addr))
        .await
        .expect("timed out connecting to router")
        .expect("failed to establish TCP connection to router");

    timeout(
        Duration::from_secs(5),
        disconnected.authenticate(&user, &pass),
    )
    .await
    .expect("timed out authenticating with router")
    .expect("failed to authenticate with valid credentials")
}

#[derive(Debug, Deserialize)]
struct Identity {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BasicResources {
    pub uptime: String,
    pub version: String,
    pub board_name: String,
    pub architecture_name: String,
    pub cpu: String,
    pub cpu_count: u8,
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_login_success() {
    let _api = connect_and_login().await;
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_login_invalid_credentials() {
    let (addr, user, _) = router_config();

    let disconnected = timeout(Duration::from_secs(5), mikrotik_api::connect(&addr))
        .await
        .expect("timed out connecting to router")
        .expect("failed to establish TCP connection to router");

    let result = timeout(
        Duration::from_secs(5),
        disconnected.authenticate(&user, "definitely-wrong-password-12345"),
    )
    .await
    .expect("timed out waiting for authentication rejection");

    assert!(
        result.is_err(),
        "expected authentication with wrong password to fail"
    );
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_fetch_router_identity() {
    let mut api = connect_and_login().await;

    let identity = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<Identity>("/system/identity/print", None),
    )
    .await
    .expect("timed out fetching router identity")
    .expect("failed to fetch router identity");

    assert!(
        !identity.name.trim().is_empty(),
        "router identity name should not be empty"
    );
    println!("Connected to router identity: '{}'", identity.name);
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_fetch_resources_uptime_and_version() {
    let mut api = connect_and_login().await;

    let res = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<BasicResources>("/system/resource/print", None),
    )
    .await
    .expect("timed out fetching basic system resources")
    .expect("failed to fetch basic system resources");

    assert!(!res.uptime.is_empty(), "uptime should not be empty");
    assert!(!res.version.is_empty(), "version should not be empty");
    assert!(!res.board_name.is_empty(), "board_name should not be empty");
    assert!(
        !res.architecture_name.is_empty(),
        "architecture_name should not be empty"
    );
    assert!(!res.cpu.is_empty(), "cpu should not be empty");
    assert!(res.cpu_count > 0, "cpu_count should be > 0");

    println!(
        "Router resources: board={}, arch={}, version={}, uptime={}, cpus={}",
        res.board_name, res.architecture_name, res.version, res.uptime, res.cpu_count
    );
}

#[tokio::test]
#[ignore = "requires live MikroTik router"]
async fn test_readonly_user_cannot_write() {
    let mut api = connect_and_login().await;

    let result = timeout(
        Duration::from_secs(5),
        api.generic_oneshot_call::<()>(
            "/system/identity/set",
            Some(&[("name", "unauthorized-change")]),
        ),
    )
    .await
    .expect("timed out waiting for router response to unauthorized write");

    assert!(
        result.is_err(),
        "expected read-only test user to be denied write permission"
    );
}
