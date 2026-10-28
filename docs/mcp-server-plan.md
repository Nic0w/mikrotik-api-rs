# Detailed Implementation Plan: `mk-mcp` (Proposal 2 — Type-State Capability Gatekeeper)

This document serves as the detailed engineering blueprint for `mk-mcp`, the Model Context Protocol (MCP) server built on top of `mikrotik-api-rs` and `rmcp`.

> **Related Issue**: Gitea Issue [#54](https://git.home.nic0w.tf/nic0w/mikrotik-api-rs/issues/54) tracks unlocking `&self` concurrent multiplexing on `MikrotikAPI<Authenticated>` by scoping the outgoing writer lock to `send_command`.

---

## 1. Architectural Overview & Core Invariants

### 1.1 Design Pillars
1. **Lean & Context-Efficient (The Tool Triad)**:
   Instead of exposing 100+ static tools that bloat the LLM context window, `mk-mcp` exposes **3 tools** (< 400 tokens total):
   - `get_overview`: 1-turn baseline orientation (identity, system resources, interfaces, IPv4/IPv6 addresses).
   - `routeros_inspect`: Dynamic schema discovery powered by RouterOS `/console/inspect` (reusing the proven models from `src/bin/tree-extractor`).
   - `routeros_exec`: Universal execution engine for queries, diagnostics (`ping`, `traceroute`), mutations, and background streaming tasks.
2. **Compile-Time Safety Gatekeeper (Type-State Pattern)**:
   - User/LLM inputs are parsed into an `UncheckedCommand`.
   - `SafetyGate::authorize(UncheckedCommand)` runs the **4-Quadrant Classifier** (`SafeRead`, `DangerousRead`, `SafeWrite`, `DangerousWrite`) and applies the server's `PolicyMode`.
   - Only `SafetyGate::authorize` can construct an `AuthorizedCommand` (its fields are private to the `safety` module).
   - `RouterDriver` methods (`execute`, `start_stream`) **only accept `AuthorizedCommand`**, making it impossible at compile time to bypass the safety layer.
3. **First-Class Streaming via MCP Tasks**:
   - Long-running or infinite RouterOS `listen` streams (`/log/listen`, `/interface/listen`, `/tool/torch`) map to the **MCP Tasks Extension (`io.modelcontextprotocol/tasks`)**, pairing `task_id` with `mikrotik-api-rs`'s multiplexed `.tag` and `api.cancel(tag)`.
4. **Pure Stdio Transport Discipline**:
   - All diagnostic logging (`log` / `simple_logger`) is routed strictly to `stderr` so `stdout` remains 100% pure JSON-RPC 2.0 frames.

---

### 1.2 End-to-End Data Flow

```
┌───────────────────────────────────────────────────────────────────────────────────┐
│                              MCP Client / AI Agent                                │
└─────────────────┬───────────────────────────────────────────────▲─────────────────┘
                  │ JSON-RPC (stdin)                              │ JSON-RPC (stdout)
┌─────────────────▼───────────────────────────────────────────────┴─────────────────┐
│ Layer 1: Agent Surface (`src/bin/mk-mcp/mcp.rs`)                                  │
│  - #[tool_router] / #[tool_handler] (`get_overview`, `inspect`, `exec`)           │
│  - Parses tool args into `UncheckedCommand`                                       │
│  - Formats router replies & `/console/inspect` trees into compact LLM content     │
└─────────────────┬───────────────────────────────────────────────▲─────────────────┘
                  │ `UncheckedCommand`                            │ `Result<AuthorizedCommand, PolicyStop>`
┌─────────────────▼───────────────────────────────────────────────┴─────────────────┐
│ Layer 2: Safety & Policy Gate (`src/bin/mk-mcp/safety/`)                          │
│  - `Classifier::classify(&UncheckedCommand)` ->                                   │
│       SafeRead | DangerousRead | SafeWrite | DangerousWrite                       │
│  - `SafetyGate::authorize(UncheckedCommand)` ->                                   │
│       Ok(AuthorizedCommand) | Err(PolicyStop::Denied | NeedsConfirmation)         │
└─────────────────┬─────────────────────────────────────────────────────────────────┘
                  │ `AuthorizedCommand` (private capability token!)
┌─────────────────▼─────────────────────────────────────────────────────────────────┐
│ Layer 3: RouterOS Driver (`src/bin/mk-mcp/router/`)                               │
│  - `RouterDriver`: Manages `MikrotikAPI<Authenticated>` & auto-reconnect          │
│  - `TaskRegistry`: Buffers `StreamingCall` events & dispatches `api.cancel(tag)`  │
└─────────────────┬─────────────────────────────────────────────────────────────────┘
                  │ TCP (8728) / TLS (8729)
┌─────────────────▼─────────────────────────────────────────────────────────────────┐
│                             MikroTik RouterOS Device                              │
└───────────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Cargo Configuration & Module Structure

### 2.1 `Cargo.toml` Additions

`mk-mcp` is gated behind an optional `mcp` feature so library-only consumers of `mikrotik_api` do not pull in MCP dependencies:

```toml
[[bin]]
name = "mk-mcp"
path = "src/bin/mk-mcp/main.rs"
required-features = ["mcp"]

[dependencies]
# Existing dependencies...
rmcp = { version = "0.1", features = ["server", "transport-io", "macros"], optional = true }
schemars = { version = "0.8", optional = true }

[features]
default = ["tls", "cli"]
tls = [
    "dep:tokio-rustls",
    "dep:rustls-pki-types",
    "dep:rustls-native-certs",
    "dep:webpki-roots",
]
cli = [
    "dep:clap",
    "dep:dialoguer",
    "dep:simple_logger",
    "dep:human_bytes",
    "dep:serde_json",
]
mcp = [
    "cli",
    "dep:rmcp",
    "dep:schemars",
]
```

### 2.2 File Tree

```text
src/bin/mk-mcp/
├── main.rs            # CLI/Env config, stderr logging init, rmcp stdio server launch
├── mcp.rs             # MkMcpServer, #[tool_router], tool schemas, inspect/overview formatters
├── safety/
│   ├── mod.rs         # SafetyClass, PolicyMode, UncheckedCommand, AuthorizedCommand, SafetyGate
│   └── classifier.rs  # Classifier trait + RuleClassifier (4-quadrant classification)
└── router/
    ├── mod.rs         # RouterDriver, connection lifecycle, AuthorizedCommand execution
    └── tasks.rs       # TaskRegistry, StreamingCall ring-buffer worker, task cancellation
```

---

## 3. Detailed Layer-by-Layer Design & Code Sketches

### 3.1 Bootstrap & Configuration (`src/bin/mk-mcp/main.rs`)

Aligns environment variables directly with `tree-extractor` and `tests/functional.rs` (`MIKROTIK_ADDR`, `MIKROTIK_USER`, `MIKROTIK_PASS`, `MIKROTIK_SSL`, `MIKROTIK_INSECURE`, `MIKROTIK_TLS_CA`, `MIKROTIK_TLS_DOMAIN`):

```rust
use clap::{Parser, ValueEnum};
use rmcp::ServiceExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PolicyMode {
    /// Only SafeRead commands are permitted (Default).
    ReadOnly,
    /// SafeRead allowed; SafeWrite requires confirmation; Dangerous commands denied.
    ConfirmWrites,
    /// SafeRead and SafeWrite allowed immediately; Dangerous commands denied.
    ReadWrite,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "mk-mcp", about = "MCP Server for MikroTik RouterOS API")]
pub struct McpConfig {
    #[arg(short = 'A', long, env = "MIKROTIK_ADDR")]
    pub address: String,

    #[arg(short = 'L', long, env = "MIKROTIK_USER")]
    pub login: String,

    #[arg(short = 'P', long, env = "MIKROTIK_PASS")]
    pub password: String,

    #[arg(long, env = "MIKROTIK_SSL", default_value_t = false)]
    pub tls: bool,

    #[arg(long, env = "MIKROTIK_INSECURE", default_value_t = false)]
    pub tls_insecure: bool,

    #[arg(long, env = "MIKROTIK_TLS_CA")]
    pub tls_ca: Option<std::path::PathBuf>,

    #[arg(long, env = "MIKROTIK_TLS_DOMAIN")]
    pub tls_domain: Option<String>,

    #[arg(long, env = "MIKROTIK_MCP_POLICY", value_enum, default_value = "read-only")]
    pub policy: PolicyMode,
}
```

---

### 3.2 Layer 2: Safety & Capability Gate (`src/bin/mk-mcp/safety/`)

#### `safety/mod.rs` — The Type-State Boundary

```rust
pub mod classifier;

use std::collections::HashMap;
pub use classifier::{Classification, Classifier, RuleClassifier};

/// The four safety quadrants every command is judged into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyClass {
    SafeRead,
    DangerousRead,
    SafeWrite,
    DangerousWrite,
}

/// Raw, unverified command assembled by the MCP tool layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncheckedCommand {
    /// Normalized slash-prefixed path, e.g. "/ip/address/print"
    pub path: String,
    /// Key-value command parameters, e.g. {"address": "1.1.1.1", "count": "4"}
    pub attributes: HashMap<String, String>,
    /// Query filters without leading '?', e.g. ["running=true", "dynamic=false"]
    pub query: Vec<String>,
    /// Optional property list, e.g. ["name", "mtu", "running"]
    pub proplist: Vec<String>,
    /// Whether this command is a streaming listener (`listen` / `torch` / `as_task`)
    pub is_streaming: bool,
}

impl UncheckedCommand {
    /// Normalizes a user path ("ip,address,print" or "ip/address/print" -> "/ip/address/print")
    pub fn new(raw_path: &str) -> Self {
        let normalized = normalize_command_path(raw_path);
        let is_streaming = normalized.ends_with("/listen");
        Self {
            path: normalized,
            attributes: HashMap::new(),
            query: Vec::new(),
            proplist: Vec::new(),
            is_streaming,
        }
    }
}

/// Sealed capability token proving a command passed `SafetyGate::authorize`.
/// Fields are private to the `safety` module so external modules cannot forge it.
#[derive(Debug, Clone)]
pub struct AuthorizedCommand {
    inner: UncheckedCommand,
    class: SafetyClass,
}

impl AuthorizedCommand {
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    pub fn class(&self) -> SafetyClass {
        self.class
    }

    pub fn is_streaming(&self) -> bool {
        self.inner.is_streaming
    }

    /// Formats attributes, query words, and `.proplist` into `mikrotik-api-rs` wire tuples.
    pub fn to_wire_attributes(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if !self.inner.proplist.is_empty() {
            out.push(("=.proplist".to_string(), self.inner.proplist.join(",")));
        }
        for q in &self.inner.query {
            let key = if q.starts_with('?') { q.clone() } else { format!("?{q}") };
            out.push((key, String::new()));
        }
        for (k, v) in &self.inner.attributes {
            out.push((k.clone(), v.clone()));
        }
        out
    }
}

/// Reason why a command was not immediately authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyStop {
    Denied {
        class: SafetyClass,
        reason: String,
    },
    NeedsConfirmation {
        class: SafetyClass,
        reason: String,
        pending: UncheckedCommand,
    },
}

pub struct SafetyGate<C = RuleClassifier> {
    mode: crate::PolicyMode,
    classifier: C,
}

impl<C: Classifier> SafetyGate<C> {
    pub fn new(mode: crate::PolicyMode, classifier: C) -> Self {
        Self { mode, classifier }
    }

    /// Evaluates an `UncheckedCommand` and mints an `AuthorizedCommand` if allowed.
    pub fn authorize(&self, cmd: UncheckedCommand) -> Result<AuthorizedCommand, PolicyStop> {
        let report = self.classifier.classify(&cmd);
        let class = report.class;

        match (self.mode, class) {
            (_, SafetyClass::SafeRead) => Ok(AuthorizedCommand { inner: cmd, class }),

            (crate::PolicyMode::ReadWrite, SafetyClass::SafeWrite) => {
                Ok(AuthorizedCommand { inner: cmd, class })
            }

            (crate::PolicyMode::ConfirmWrites, SafetyClass::SafeWrite) => {
                Err(PolicyStop::NeedsConfirmation {
                    class,
                    reason: report.reason.unwrap_or_else(|| {
                        format!("Write command '{}' requires user confirmation", cmd.path)
                    }),
                    pending: cmd,
                })
            }

            (crate::PolicyMode::ReadOnly, SafetyClass::SafeWrite) => Err(PolicyStop::Denied {
                class,
                reason: format!(
                    "Command '{}' is classified as SafeWrite, but server is in read-only mode",
                    cmd.path
                ),
            }),

            (_, SafetyClass::DangerousRead | SafetyClass::DangerousWrite) => {
                Err(PolicyStop::Denied {
                    class,
                    reason: report.reason.unwrap_or_else(|| {
                        format!("Command '{}' is blocked ({:?})", cmd.path, class)
                    }),
                })
            }
        }
    }
}
```

#### `safety/classifier.rs` — The Black-Box Classifier Interface

```rust
use super::{SafetyClass, UncheckedCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub class: SafetyClass,
    pub reason: Option<String>,
}

/// Pluggable black-box classifier trait.
pub trait Classifier: Send + Sync {
    fn classify(&self, cmd: &UncheckedCommand) -> Classification;
}

/// Deterministic baseline classifier (fail-closed on unknown verbs).
#[derive(Debug, Default, Clone)]
pub struct RuleClassifier;

impl Classifier for RuleClassifier {
    fn classify(&self, cmd: &UncheckedCommand) -> Classification {
        let path = cmd.path.as_str();
        let verb = path.rsplit('/').next().unwrap_or("");

        // 1. Check for DangerousWrite patterns first (highest priority)
        if is_catastrophic_write(path, &cmd.attributes) {
            return Classification {
                class: SafetyClass::DangerousWrite,
                reason: Some("Catastrophic or lockout-prone operation".into()),
            };
        }

        // 2. Check for DangerousRead patterns (secret leakage or unbounded network flood)
        if let Some(reason) = is_dangerous_read(path, &cmd.attributes) {
            return Classification {
                class: SafetyClass::DangerousRead,
                reason: Some(reason),
            };
        }

        // 3. Classify by command verb / path
        match verb {
            "print" | "get" | "find" | "listen" | "monitor" | "monitor-traffic" | "inspect" => {
                Classification {
                    class: SafetyClass::SafeRead,
                    reason: None,
                }
            }
            "ping" | "traceroute" | "dns-lookup" => Classification {
                class: SafetyClass::SafeRead,
                reason: None,
            },
            "add" | "set" | "remove" | "enable" | "disable" | "comment" | "unset" => {
                Classification {
                    class: SafetyClass::SafeWrite,
                    reason: None,
                }
            }
            // Fail-closed: unknown verbs default to DangerousWrite
            _ => Classification {
                class: SafetyClass::DangerousWrite,
                reason: Some(format!("Unrecognized or privileged command verb '{verb}'")),
            },
        }
    }
}
```

---

### 3.3 Layer 3: RouterOS Driver & Task Manager (`src/bin/mk-mcp/router/`)

#### `router/mod.rs` — Connection Lifecycle & Execution

```rust
pub mod tasks;

use std::{collections::HashMap, sync::Arc};
use mikrotik_api::{Authenticated, Error as ApiError, MikrotikAPI};
use tokio::sync::Mutex;
use crate::safety::AuthorizedCommand;
use self::tasks::TaskRegistry;

pub type Row = HashMap<String, String>;

#[derive(Clone)]
pub struct RouterDriver {
    conn: Arc<Mutex<Option<MikrotikAPI<Authenticated>>>>,
    connector: Arc<dyn RouterConnector>,
    tasks: TaskRegistry,
}

#[async_trait::async_trait]
pub trait RouterConnector: Send + Sync {
    async fn connect(&self) -> Result<MikrotikAPI<Authenticated>, ApiError>;
}

impl RouterDriver {
    pub fn new(connector: Arc<dyn RouterConnector>) -> Self {
        Self {
            conn: Arc::new(Mutex::new(None)),
            connector,
            tasks: TaskRegistry::new(),
        }
    }

    pub fn tasks(&self) -> &TaskRegistry {
        &self.tasks
    }

    /// Executes a finite `AuthorizedCommand` (`SafeRead` or `SafeWrite`).
    /// Notice: Cannot be called without an `AuthorizedCommand` token!
    pub async fn execute(&self, cmd: AuthorizedCommand) -> Result<Vec<Row>, ApiError> {
        let wire_owned = cmd.to_wire_attributes();
        let wire_refs: Vec<(&str, &str)> = wire_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let attrs = if wire_refs.is_empty() {
            None
        } else {
            Some(wire_refs.as_slice())
        };

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connector.connect().await?);
        }

        let api = guard.as_mut().expect("connected");
        match api.generic_array_call::<Row>(cmd.path(), attrs).await {
            Ok(rows) => Ok(rows),
            Err(ApiError::EndOfStream | ApiError::Io(_)) => {
                // Connection dropped; clear stale session
                *guard = None;
                // Auto-reconnect once if the command is idempotent (SafeRead)
                if cmd.class() == crate::safety::SafetyClass::SafeRead {
                    let mut fresh_api = self.connector.connect().await?;
                    let res = fresh_api.generic_array_call::<Row>(cmd.path(), attrs).await;
                    *guard = Some(fresh_api);
                    res
                } else {
                    Err(ApiError::EndOfStream)
                }
            }
            Err(other) => Err(other),
        }
    }

    /// Starts a background streaming command (`StreamingCall`) and registers an MCP Task.
    pub async fn start_stream(&self, cmd: AuthorizedCommand) -> Result<String, ApiError> {
        let wire_owned = cmd.to_wire_attributes();
        let wire_refs: Vec<(&str, &str)> = wire_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let attrs = if wire_refs.is_empty() {
            None
        } else {
            Some(wire_refs.as_slice())
        };

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connector.connect().await?);
        }

        let api = guard.as_mut().expect("connected");
        let mut tag: u16 = 0;
        let stream = api
            .generic_streaming_call::<Row>(cmd.path(), attrs, &mut tag)
            .await;

        if tag == 0 {
            return Err(ApiError::EndOfStream);
        }

        let task_id = self.tasks.spawn_stream_task(cmd.path().to_string(), tag, stream);
        Ok(task_id)
    }

    /// Cancels an active streaming MCP Task by sending `/cancel =tag=<u16>` to the router.
    pub async fn cancel_stream(&self, task_id: &str) -> Result<bool, ApiError> {
        if let Some(tag) = self.tasks.mark_cancelling(task_id) {
            let mut guard = self.conn.lock().await;
            if let Some(api) = guard.as_mut() {
                let _ = api.cancel(tag).await;
            }
            self.tasks.finish_cancel(task_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
```

#### `router/tasks.rs` — Bounded Ring-Buffer Task Registry

```rust
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use futures::{Stream, StreamExt};
use mikrotik_api::Response;
use tokio::task::JoinHandle;
use super::Row;

const MAX_BUFFERED_EVENTS_PER_TASK: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, serde::Serialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub command: String,
    pub tag: u16,
    pub status: TaskStatus,
    pub events: Vec<Row>,
    pub dropped_events: usize,
    pub error: Option<String>,
}

struct TaskEntry {
    command: String,
    tag: u16,
    status: TaskStatus,
    buffer: VecDeque<Row>,
    dropped_events: usize,
    error: Option<String>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<Mutex<HashMap<String, TaskEntry>>>,
    next_id: Arc<AtomicU64>,
}
```
* **Bounded Memory Guarantee**: Each task buffers at most `MAX_BUFFERED_EVENTS_PER_TASK` (500) events in a `VecDeque`. If the agent polls slowly on a noisy `/log/listen` stream, the oldest events roll off and increment `dropped_events`, guaranteeing the MCP server never leaks memory.

---

### 3.4 Layer 1: Agent Surface & Tool Triad (`src/bin/mk-mcp/mcp.rs`)

```rust
use std::{collections::HashMap, sync::Arc};
use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    ErrorData as McpError, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use crate::{
    router::RouterDriver,
    safety::{PolicyStop, SafetyGate, UncheckedCommand},
};

#[derive(Clone)]
pub struct MkMcpServer {
    safety: Arc<SafetyGate>,
    driver: RouterDriver,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectArgs {
    /// Menu or command path (e.g. "/ip/firewall/filter" or "ip,address,add")
    pub path: Option<String>,
    /// Include syntax descriptions for commands and arguments (default: true)
    #[serde(default = "default_true")]
    pub include_docs: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecArgs {
    /// Full RouterOS command path (e.g. "/ip/address/print", "/ping", "/log/listen")
    pub command: String,
    /// Key-value parameters (e.g. {"address": "1.1.1.1", "count": "4"})
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    /// Query filters (e.g. ["running=true", "dynamic=false"])
    #[serde(default)]
    pub query: Vec<String>,
    /// Optional list of properties to return (e.g. ["name", "mac-address", "running"])
    #[serde(default)]
    pub proplist: Vec<String>,
    /// Run as a background streaming task (automatic for */listen commands)
    #[serde(default)]
    pub as_task: bool,
}

#[tool_router]
impl MkMcpServer {
    pub fn new(safety: Arc<SafetyGate>, driver: RouterDriver) -> Self {
        Self {
            safety,
            driver,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Retrieve a fast, 1-turn overview of the router: identity, hardware resources, interfaces, and IP addresses.")]
    pub async fn get_overview(&self) -> Result<CallToolResult, McpError> {
        // Uses SafetyGate::authorize on the 4 baseline SafeRead queries:
        // 1. /system/identity/print
        // 2. /system/resource/print
        // 3. /interface/print (proplist: name,type,running,disabled,mtu,mac-address)
        // 4. /ip/address/print (proplist: address,network,interface,disabled,dynamic)
        ...
    }

    #[tool(description = "Inspect RouterOS command hierarchy, submenus, valid arguments, and syntax documentation using /console/inspect.")]
    pub async fn routeros_inspect(
        &self,
        Parameters(args): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Calls /console/inspect with request=child (and request=syntax if include_docs=true)
        // Compacts into { path, directories: [...], commands: [...], arguments: [...] }
        ...
    }

    #[tool(description = "Execute a RouterOS API command (query, diagnostic, mutation, or streaming listener). Subject to the safety classifier.")]
    pub async fn routeros_exec(
        &self,
        Parameters(args): Parameters<ExecArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut unchecked = UncheckedCommand::new(&args.command);
        unchecked.attributes = args.attributes;
        unchecked.query = args.query;
        unchecked.proplist = args.proplist;
        if args.as_task {
            unchecked.is_streaming = true;
        }

        let authorized = match self.safety.authorize(unchecked) {
            Ok(auth) => auth,
            Err(PolicyStop::Denied { class, reason }) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Safety Policy Denied ({class:?}): {reason}"
                ))]));
            }
            Err(PolicyStop::NeedsConfirmation { class, reason, pending }) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Confirmation Required ({class:?}): {reason} (path: {})",
                    pending.path
                ))]));
            }
        };

        if authorized.is_streaming() {
            let task_id = self.driver.start_stream(authorized).await.map_err(to_mcp_err)?;
            Ok(CallToolResult::success(vec![Content::json(serde_json::json!({
                "status": "working",
                "task_id": task_id,
                "message": "Streaming task started. Poll or cancel using task_id."
            }))?]))
        } else {
            let rows = self.driver.execute(authorized).await.map_err(to_mcp_err)?;
            Ok(CallToolResult::success(vec![Content::json(rows)?]))
        }
    }
}
```

---

## 4. Comprehensive Testing Strategy

To keep quality high and iteration fast, testing is structured into **three tiers**:

### Tier 1: Pure Unit Tests (In-Memory, Zero I/O, < 5ms)
Located inside `src/bin/mk-mcp/safety/` and `src/bin/mk-mcp/mcp.rs`:
1. **Path Normalization & Wire Formatting Tests**:
   - Verify `"ip,address,print"`, `"ip/address/print"`, and `"/ip/address/print"` all normalize to `"/ip/address/print"`.
   - Verify `proplist: ["name", "mtu"]` formats to `("=.proplist", "name,mtu")` and `query: ["running=true"]` formats to `("?running=true", "")`.
2. **4-Quadrant Classifier Matrix Tests (`safety/classifier.rs`)**:
   - **`SafeRead`**: `/interface/print`, `/ip/address/print`, `/console/inspect`, `/ping` (with `count <= 10`), `/log/listen`.
   - **`DangerousRead`**: `/export` with `show-sensitive=yes`, `/user/ssh-keys/private/print`, `/tool/bandwidth-test` without duration limit.
   - **`SafeWrite`**: `/ip/dhcp-server/lease/add`, `/ip/firewall/filter/set` (updating `comment`), `/interface/ethernet/set`.
   - **`DangerousWrite`**: `/system/reset-configuration`, `/system/reboot`, `/system/shutdown`, `/user/remove`, `/ip/service/disable` on `api`/`api-ssl`, and unknown verbs (verifying fail-closed behavior).
3. **Policy Gate State Machine Tests (`safety/mod.rs`)**:
   - Exhaustive test of the $3 \times 4$ matrix (`ReadOnly`, `ConfirmWrites`, `ReadWrite` $\times$ `SafeRead`, `DangerousRead`, `SafeWrite`, `DangerousWrite`), verifying when `Ok(AuthorizedCommand)`, `Err(PolicyStop::NeedsConfirmation)`, or `Err(PolicyStop::Denied)` is returned.

### Tier 2: Deterministic Mock-Router Integration Tests (`tokio::io::duplex`)
Using `MikrotikAPI::from_stream` over `tokio::io::duplex(8192)` with a lightweight scripted mock RouterOS peer (zero network sockets required):
1. **`get_overview` Batch Test**:
   - Mock router responds to `/system/identity/print`, `/system/resource/print`, `/interface/print`, and `/ip/address/print`.
   - Assert `get_overview` returns a structured JSON summary with identity, CPU/RAM, interfaces, and IPs.
2. **`routeros_inspect` Compaction Test**:
   - Mock router returns `InspectEntry` (`dir`, `cmd`, `arg`) and `SyntaxEntry` frames.
   - Assert `routeros_inspect` strips empty/internal entries and groups directories, commands, and arguments with descriptions.
3. **Safety Gate End-to-End Enforcement Test**:
   - Call `routeros_exec` with `/system/reset-configuration`.
   - Assert `CallToolResult::is_error == Some(true)` and verify **zero bytes were written to the mock router stream**.
4. **Streaming Task Lifecycle & Ring-Buffer Test**:
   - Call `routeros_exec` with `/log/listen`.
   - Mock router streams 5 `!re` log sentences.
   - Poll task snapshot and verify all 5 events are returned in order.
   - Trigger task cancellation and assert the mock router receives `/cancel` with `=tag=<assigned_tag>`.
5. **Auto-Reconnect on Dropped Connection Test**:
   - Mock router closes stream #1 after authentication; `RouterDriver` executes a `SafeRead`, detects `Error::EndOfStream`, transparently connects to stream #2, and returns the reply.

### Tier 3: Live RouterOS Functional Tests (`tests/mcp_functional.rs`)
Guarded by `MIKROTIK_ADDR`, `MIKROTIK_USER`, `MIKROTIK_PASS` (matching `tests/functional.rs`):
1. Connect `MkMcpServer` to the live test router.
2. Execute `get_overview` and validate non-empty board name, RouterOS version, and interface list.
3. Execute `routeros_inspect(path: "ip,address")` and verify `print`, `add`, `remove`, `set` commands are discovered dynamically from the live router.
4. Execute `routeros_exec(command: "/interface/print", proplist: ["name", "type"], query: ["running=true"])` and verify filtered properties.
5. Start a live `/interface/listen` task, poll its status (`working`), cancel it, and verify `api.active_calls_count() == 0`.

---

## 5. Phased Implementation Sequence

1. **Step 1 — Cargo & Safety Core**:
   - Add `mcp` feature to `Cargo.toml`.
   - Implement `src/bin/mk-mcp/safety/mod.rs` and `src/bin/mk-mcp/safety/classifier.rs` + Tier 1 unit tests.
2. **Step 2 — RouterDriver & TaskRegistry**:
   - Implement `src/bin/mk-mcp/router/mod.rs` and `src/bin/mk-mcp/router/tasks.rs` + Tier 2 mock-stream tests.
3. **Step 3 — `rmcp` Server & Tool Triad**:
   - Implement `src/bin/mk-mcp/mcp.rs` and `src/bin/mk-mcp/main.rs` (`get_overview`, `routeros_inspect`, `routeros_exec`).
4. **Step 4 — Functional Verification & Polish**:
   - Add `tests/mcp_functional.rs`, run `cargo test --all-targets --features mcp`, `cargo clippy --all-targets --features mcp`, and `cargo fmt -- --check`.
