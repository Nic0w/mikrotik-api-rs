pub mod mcp;
pub mod router;
pub mod safety;

#[cfg(test)]
mod mock_test;

use std::{path::PathBuf, sync::Arc};

use clap::Parser;
use log::info;
use mcp::MkMcpServer;
use rmcp::ServiceExt;
use router::{LiveConnector, RouterDriver};
use safety::{PolicyMode, RuleClassifier, SafetyGate};

/// Command-line configuration for `mk-mcp`.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "mk-mcp",
    about = "Model Context Protocol (MCP) server for MikroTik RouterOS API"
)]
pub struct McpConfig {
    /// Router address in <HOST>:<PORT> format (default port: 8728 for plain, 8729 for TLS).
    #[arg(short = 'A', long, env = "MIKROTIK_ADDR")]
    pub address: String,

    /// Router API username.
    #[arg(short = 'L', long, env = "MIKROTIK_USER")]
    pub login: String,

    /// Router API password.
    #[arg(short = 'P', long, env = "MIKROTIK_PASS")]
    pub password: String,

    /// Connect via TLS (port 8729).
    #[arg(long, env = "MIKROTIK_SSL", default_value_t = false)]
    pub tls: bool,

    /// Accept self-signed or invalid TLS certificates (insecure).
    #[arg(long, env = "MIKROTIK_INSECURE", default_value_t = false)]
    pub tls_insecure: bool,

    /// Path to a custom Certificate Authority (CA) certificate file for TLS validation.
    #[arg(long, env = "MIKROTIK_TLS_CA")]
    pub tls_ca: Option<PathBuf>,

    /// Override SNI domain name for TLS validation.
    #[arg(long, env = "MIKROTIK_TLS_DOMAIN")]
    pub tls_domain: Option<String>,

    /// Safety policy mode: read-only, confirm-writes, or read-write.
    #[arg(
        long,
        env = "MIKROTIK_MCP_POLICY",
        value_enum,
        default_value = "read-only"
    )]
    pub policy: PolicyMode,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Crucial: Route all logs strictly to stderr so stdout is reserved for JSON-RPC frames.
    let _ = simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .init();

    let config = McpConfig::parse();

    info!(
        "Starting mk-mcp connecting to {} (policy: {:?}, TLS: {})...",
        config.address, config.policy, config.tls
    );

    let safety_gate = Arc::new(SafetyGate::new(config.policy, RuleClassifier));

    let connector = Arc::new(LiveConnector::new(
        config.address,
        config.login,
        config.password,
        config.tls,
        config.tls_insecure,
        config.tls_ca,
        config.tls_domain,
    ));

    let driver = RouterDriver::new(connector);
    let server = MkMcpServer::new(safety_gate, driver);

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = server.serve(transport).await?;
    running.waiting().await?;

    Ok(())
}
