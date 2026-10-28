use std::{collections::HashMap, sync::Arc};

use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars::JsonSchema,
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};

use crate::{
    router::{RouterDriver, Row, TaskStatus},
    safety::{PolicyStop, SafetyGate, UncheckedCommand},
};

fn default_true() -> bool {
    true
}

/// Arguments for `routeros_inspect` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct InspectArgs {
    /// RouterOS menu or command path to inspect (e.g. "ip,address" or "/ip/firewall/filter"). Defaults to root.
    #[serde(default)]
    pub path: Option<String>,
    /// Whether to fetch syntax descriptions for subcommands and arguments (default: true).
    #[serde(default = "default_true")]
    pub include_docs: bool,
}

/// Action to perform on an active background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskAction {
    /// Read new buffered events from the task.
    Poll,
    /// Cancel the streaming task and stop router sentence generation.
    Cancel,
}

/// Arguments for `routeros_exec` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ExecArgs {
    /// Full RouterOS command path (e.g. "/ip/address/print", "/ping", "/log/listen").
    #[serde(default)]
    pub command: Option<String>,
    /// Key-value command attributes (e.g. {"address": "1.1.1.1", "count": "4"}).
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    /// Query filter words (e.g. ["running=true", "dynamic=false"]).
    #[serde(default)]
    pub query: Vec<String>,
    /// Specific property fields to retrieve (e.g. ["name", "mac-address", "running"]).
    #[serde(default)]
    pub proplist: Vec<String>,
    /// Run as a background streaming MCP Task (automatic for */listen commands).
    #[serde(default)]
    pub as_task: bool,
    /// Optional active task identifier to poll or cancel.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Action to execute on `task_id` (poll or cancel).
    #[serde(default)]
    pub task_action: Option<TaskAction>,
}

/// Item metadata discovered via `/console/inspect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectItem {
    /// Item name.
    pub name: String,
    /// Item description if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Compact inspection summary returned to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectResult {
    /// Inspected path.
    pub path: String,
    /// Submenus and directories.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub directories: Vec<String>,
    /// Commands available at this path.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub commands: Vec<InspectItem>,
    /// Parameters and arguments for this command/menu.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub arguments: Vec<InspectItem>,
}

/// High-level 1-turn router overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterOverview {
    /// Router identity name.
    pub identity: String,
    /// Hardware and operating system resource metrics.
    pub resources: Row,
    /// Summary of network interfaces.
    pub interfaces: Vec<Row>,
    /// Configured IP addresses.
    pub ip_addresses: Vec<Row>,
}

/// The core MCP Server implementation wrapping `rmcp`.
#[derive(Clone)]
pub struct MkMcpServer {
    safety: Arc<SafetyGate>,
    driver: RouterDriver,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MkMcpServer {
    /// Creates a new `MkMcpServer`.
    pub fn new(safety: Arc<SafetyGate>, driver: RouterDriver) -> Self {
        Self {
            safety,
            driver,
            tool_router: Self::tool_router(),
        }
    }

    /// Retrieve a fast, 1-turn baseline overview of the router (identity, system resources, interfaces, and IP addresses).
    #[tool(
        description = "Retrieve a fast, 1-turn baseline overview of the router: identity, hardware resources (CPU, RAM, version), interfaces, and configured IP addresses."
    )]
    pub async fn get_overview(&self) -> Result<CallToolResult, McpError> {
        let mut identity = "unknown".to_string();
        let mut resources = Row::new();
        let mut interfaces = Vec::new();
        let mut ip_addresses = Vec::new();

        // 1. Identity
        let id_cmd = UncheckedCommand::new("/system/identity/print");
        if let Ok(auth) = self.safety.authorize(id_cmd) {
            if let Ok(rows) = self.driver.execute(auth).await {
                if let Some(first) = rows.first() {
                    if let Some(name) = first.get("name") {
                        identity = name.clone();
                    }
                }
            }
        }

        // 2. Resources
        let res_cmd = UncheckedCommand::new("/system/resource/print");
        if let Ok(auth) = self.safety.authorize(res_cmd) {
            if let Ok(rows) = self.driver.execute(auth).await {
                if let Some(first) = rows.into_iter().next() {
                    resources = first;
                }
            }
        }

        // 3. Interfaces (compact proplist)
        let mut iface_cmd = UncheckedCommand::new("/interface/print");
        iface_cmd.proplist = vec![
            "name".into(),
            "type".into(),
            "running".into(),
            "disabled".into(),
            "mtu".into(),
            "mac-address".into(),
            "comment".into(),
        ];
        if let Ok(auth) = self.safety.authorize(iface_cmd) {
            if let Ok(rows) = self.driver.execute(auth).await {
                interfaces = rows;
            }
        }

        // 4. IP Addresses
        let mut ip_cmd = UncheckedCommand::new("/ip/address/print");
        ip_cmd.proplist = vec![
            "address".into(),
            "network".into(),
            "interface".into(),
            "disabled".into(),
            "dynamic".into(),
        ];
        if let Ok(auth) = self.safety.authorize(ip_cmd) {
            if let Ok(rows) = self.driver.execute(auth).await {
                ip_addresses = rows;
            }
        }

        let overview = RouterOverview {
            identity,
            resources,
            interfaces,
            ip_addresses,
        };

        match ContentBlock::json(overview) {
            Ok(block) => Ok(CallToolResult::success(vec![block])),
            Err(e) => Err(e),
        }
    }

    /// Inspect RouterOS command hierarchy, child submenus, valid arguments, and syntax documentation using /console/inspect.
    #[tool(
        description = "Inspect RouterOS command hierarchy, child submenus, valid arguments, and syntax documentation using /console/inspect. Discovers exact parameter names and types on demand."
    )]
    pub async fn routeros_inspect(
        &self,
        Parameters(args): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, McpError> {
        let raw_path = args.path.unwrap_or_default();
        let normalized = raw_path.trim().replace('/', ",");
        let query_path = normalized
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join(",");

        // 1. Fetch children
        let mut inspect_cmd = UncheckedCommand::new("/console/inspect");
        inspect_cmd
            .attributes
            .insert("request".to_string(), "child".to_string());
        if !query_path.is_empty() {
            inspect_cmd
                .attributes
                .insert("path".to_string(), query_path.clone());
        }

        let auth = match self.safety.authorize(inspect_cmd) {
            Ok(a) => a,
            Err(PolicyStop::Denied { reason, .. }) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Inspection blocked: {reason}"
                ))]));
            }
            Err(PolicyStop::NeedsConfirmation { reason, .. }) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Inspection blocked: {reason}"
                ))]));
            }
        };

        let rows = match self.driver.execute(auth).await {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Failed to query /console/inspect: {e}"
                ))]));
            }
        };

        // 2. Fetch docs if requested
        let mut docs: HashMap<String, String> = HashMap::new();
        if args.include_docs && !query_path.is_empty() {
            let mut syntax_cmd = UncheckedCommand::new("/console/inspect");
            syntax_cmd
                .attributes
                .insert("request".to_string(), "syntax".to_string());
            syntax_cmd
                .attributes
                .insert("path".to_string(), query_path.clone());
            if let Ok(syntax_auth) = self.safety.authorize(syntax_cmd) {
                if let Ok(syntax_rows) = self.driver.execute(syntax_auth).await {
                    for row in syntax_rows {
                        if let (Some(symbol), Some(text)) = (row.get("symbol"), row.get("text")) {
                            if !text.trim().is_empty() {
                                docs.insert(symbol.clone(), text.clone());
                            }
                        }
                    }
                }
            }
        }

        let mut directories = Vec::new();
        let mut commands = Vec::new();
        let mut arguments = Vec::new();

        for row in rows {
            let record_type = row.get("type").map(String::as_str).unwrap_or("");
            if record_type != "child" {
                continue;
            }

            let name = row.get("name").cloned().unwrap_or_default();
            if name.is_empty() {
                continue;
            }

            let node_type = row.get("node-type").map(String::as_str).unwrap_or("");
            let description = docs.get(&name).cloned();

            match node_type {
                "dir" | "path" => directories.push(name),
                "cmd" => commands.push(InspectItem { name, description }),
                "arg" => arguments.push(InspectItem { name, description }),
                _ => {}
            }
        }

        let result = InspectResult {
            path: if query_path.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", query_path.replace(',', "/"))
            },
            directories,
            commands,
            arguments,
        };

        match ContentBlock::json(result) {
            Ok(block) => Ok(CallToolResult::success(vec![block])),
            Err(e) => Err(e),
        }
    }

    /// Execute a RouterOS API command (query, diagnostic, mutation, or streaming task).
    #[tool(
        description = "Execute a RouterOS command. Can execute queries (/ip/address/print), action commands (/ping), mutations, or manage background streaming tasks (/log/listen)."
    )]
    pub async fn routeros_exec(
        &self,
        Parameters(args): Parameters<ExecArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Handle task management (poll/cancel) if requested
        if let Some(task_id) = &args.task_id {
            match args.task_action {
                Some(TaskAction::Cancel) => match self.driver.cancel_stream(task_id).await {
                    Ok(true) => {
                        return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                            "Task '{task_id}' was cancelled."
                        ))]));
                    }
                    Ok(false) => {
                        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "Task '{task_id}' was not found or already stopped."
                        ))]));
                    }
                    Err(e) => {
                        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "Failed to cancel task '{task_id}': {e}"
                        ))]));
                    }
                },
                Some(TaskAction::Poll) | None => {
                    if let Some(snapshot) = self.driver.tasks().poll_task(task_id, true) {
                        return match ContentBlock::json(snapshot) {
                            Ok(block) => Ok(CallToolResult::success(vec![block])),
                            Err(e) => Err(e),
                        };
                    } else {
                        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "Task '{task_id}' not found."
                        ))]));
                    }
                }
            }
        }

        let Some(command_path) = args.command else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Either 'command' or 'task_id' must be provided.",
            )]));
        };

        let mut unchecked = UncheckedCommand::new(&command_path);
        unchecked.attributes = args.attributes;
        unchecked.query = args.query;
        unchecked.proplist = args.proplist;
        if args.as_task {
            unchecked.is_streaming = true;
        }

        let authorized = match self.safety.authorize(unchecked) {
            Ok(auth) => auth,
            Err(PolicyStop::Denied { class, reason }) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Safety Policy Denied ({class:?}): {reason}"
                ))]));
            }
            Err(PolicyStop::NeedsConfirmation {
                class,
                reason,
                pending,
            }) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Confirmation Required ({class:?}): {reason} (Command: '{}')",
                    pending.path
                ))]));
            }
        };

        if authorized.is_streaming() {
            match self.driver.start_stream(authorized).await {
                Ok(task_id) => {
                    let resp = serde_json::json!({
                        "status": TaskStatus::Working,
                        "task_id": task_id,
                        "message": "Streaming task started. Poll with routeros_exec(task_id=..., task_action='poll') or cancel with task_action='cancel'."
                    });
                    match ContentBlock::json(resp) {
                        Ok(b) => Ok(CallToolResult::success(vec![b])),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Failed to start stream: {e}"
                ))])),
            }
        } else {
            match self.driver.execute(authorized).await {
                Ok(rows) => match ContentBlock::json(rows) {
                    Ok(b) => Ok(CallToolResult::success(vec![b])),
                    Err(e) => Err(e),
                },
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Command failed on router: {e}"
                ))])),
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for MkMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("MikroTik RouterOS API MCP server providing fast, progressive-disclosure network observability and configuration tools.")
    }
}
