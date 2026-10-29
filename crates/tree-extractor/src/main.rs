use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::PathBuf,
    time::Instant,
};

use clap::{Parser, ValueEnum};
use dialoguer::Password;
use mikrotik_api::{Authenticated, MikrotikAPI};
use serde::{Deserialize, Serialize};

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Tree,
    Json,
    Summary,
}

#[derive(Parser, Debug)]
#[command(
    name = "tree-extractor",
    about = "Extract and inspect the complete RouterOS CLI command hierarchy using /console/inspect"
)]
struct Cli {
    #[arg(
        short = 'A',
        long,
        env = "MIKROTIK_ADDR",
        help = "Router address in <HOST>:<PORT> format"
    )]
    address: String,

    #[arg(short = 'L', long, env = "MIKROTIK_USER", help = "Router username")]
    login: String,

    #[arg(short = 'P', long, env = "MIKROTIK_PASS", help = "Router password")]
    password: Option<String>,

    #[arg(
        long,
        help = "Subtree path to extract (comma-separated, e.g. 'ip,address' or 'system'). Defaults to root."
    )]
    path: Option<String>,

    #[arg(long, help = "Maximum depth to recurse into subdirectories")]
    max_depth: Option<usize>,

    #[arg(long, help = "Do not extract command arguments")]
    no_args: bool,

    #[arg(
        long,
        help = "Query syntax help strings and docstrings for arguments and commands"
    )]
    include_docs: bool,

    #[arg(
        short = 'f',
        long,
        value_enum,
        default_value = "tree",
        help = "Output format"
    )]
    format: OutputFormat,

    #[arg(short = 'o', long, help = "Write output to file instead of stdout")]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct InspectEntry {
    #[serde(rename = "type")]
    pub record_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub node_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SyntaxEntry {
    #[serde(rename = "type")]
    pub record_type: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub node_type: String, // "root", "dir", "path", "cmd", "arg"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn new(name: String, node_type: String) -> Self {
        Self {
            name,
            node_type,
            description: None,
            children: Vec::new(),
        }
    }

    pub fn count_stats(&self) -> (usize, usize, usize) {
        // (dirs/paths, cmds, args)
        let mut dirs = 0;
        let mut cmds = 0;
        let mut args = 0;

        match self.node_type.as_str() {
            "dir" | "path" | "root" => dirs += 1,
            "cmd" => cmds += 1,
            "arg" => args += 1,
            _ => {}
        }

        for child in &self.children {
            let (d, c, a) = child.count_stats();
            dirs += d;
            cmds += c;
            args += a;
        }

        (dirs, cmds, args)
    }

    pub fn print_tree(&self, prefix: &str, is_last: bool, is_root: bool, out: &mut String) {
        let (branch, next_prefix) = if is_root {
            ("", prefix.to_string())
        } else if is_last {
            ("└── ", format!("{prefix}    "))
        } else {
            ("├── ", format!("{prefix}│   "))
        };

        let type_suffix = match self.node_type.as_str() {
            "dir" => " [dir]",
            "path" => " [path]",
            "cmd" => " (cmd)",
            "arg" => " <arg>",
            _ => "",
        };

        let desc_suffix = match &self.description {
            Some(d) if !d.is_empty() => format!(" - {d}"),
            _ => String::new(),
        };

        out.push_str(&format!(
            "{prefix}{branch}{}{type_suffix}{desc_suffix}\n",
            self.name
        ));

        let total = self.children.len();
        for (i, child) in self.children.iter().enumerate() {
            let last = i + 1 == total;
            child.print_tree(&next_prefix, last, false, out);
        }
    }
}

async fn fetch_docs_map(
    api: &mut MikrotikAPI<Authenticated>,
    path: &str,
) -> HashMap<String, String> {
    let mut docs = HashMap::new();
    let attrs = [("request", "syntax"), ("path", path)];
    if let Ok(replies) = api
        .generic_array_call::<SyntaxEntry>("/console/inspect", Some(&attrs))
        .await
    {
        for entry in replies {
            if let Some(text) = entry.text {
                if !text.trim().is_empty() {
                    docs.insert(entry.symbol, text);
                }
            }
        }
    }
    docs
}

#[derive(Debug, Clone, Copy)]
struct ExtractConfig {
    max_depth: Option<usize>,
    include_args: bool,
    include_docs: bool,
}

async fn extract_subtree(
    api: &mut MikrotikAPI<Authenticated>,
    current_path: &str,
    node_name: &str,
    node_type: &str,
    current_depth: usize,
    config: ExtractConfig,
    visited: &mut HashSet<String>,
) -> TreeNode {
    let mut node = TreeNode::new(node_name.to_string(), node_type.to_string());

    if let Some(limit) = config.max_depth {
        if current_depth >= limit {
            return node;
        }
    }

    // Fetch children
    let mut attrs = vec![("request", "child")];
    if !current_path.is_empty() {
        attrs.push(("path", current_path));
    }

    let entries = match api
        .generic_array_call::<InspectEntry>("/console/inspect", Some(&attrs))
        .await
    {
        Ok(res) => res,
        Err(e) => {
            eprintln!("Warning: failed to query path '{}': {}", current_path, e);
            return node;
        }
    };

    // Optionally fetch docstrings for this menu level
    let docs = if config.include_docs && !current_path.is_empty() {
        fetch_docs_map(api, current_path).await
    } else {
        HashMap::new()
    };

    for entry in entries {
        if entry.record_type != "child" || entry.name.is_empty() {
            continue;
        }

        let child_path = if current_path.is_empty() {
            entry.name.clone()
        } else {
            format!("{},{}", current_path, entry.name)
        };

        match entry.node_type.as_str() {
            "dir" | "path" => {
                if visited.insert(child_path.clone()) {
                    let child_node = Box::pin(extract_subtree(
                        api,
                        &child_path,
                        &entry.name,
                        &entry.node_type,
                        current_depth + 1,
                        config,
                        visited,
                    ))
                    .await;
                    node.children.push(child_node);
                }
            }
            "cmd" => {
                let mut cmd_node = TreeNode::new(entry.name.clone(), "cmd".to_string());
                if let Some(desc) = docs.get(&entry.name) {
                    cmd_node.description = Some(desc.clone());
                }

                if config.include_args {
                    // Extract arguments of this command
                    let cmd_attrs = [("request", "child"), ("path", &child_path)];
                    if let Ok(arg_entries) = api
                        .generic_array_call::<InspectEntry>("/console/inspect", Some(&cmd_attrs))
                        .await
                    {
                        let arg_docs = if config.include_docs {
                            fetch_docs_map(api, &child_path).await
                        } else {
                            HashMap::new()
                        };

                        for arg in arg_entries {
                            if arg.record_type == "child" && arg.node_type == "arg" {
                                let mut arg_node =
                                    TreeNode::new(arg.name.clone(), "arg".to_string());
                                if let Some(d) = arg_docs.get(&arg.name) {
                                    arg_node.description = Some(d.clone());
                                }
                                cmd_node.children.push(arg_node);
                            }
                        }
                    }
                }

                node.children.push(cmd_node);
            }
            "arg" => {
                let mut arg_node = TreeNode::new(entry.name.clone(), "arg".to_string());
                if let Some(d) = docs.get(&entry.name) {
                    arg_node.description = Some(d.clone());
                }
                node.children.push(arg_node);
            }
            other => {
                let unk = TreeNode::new(entry.name, other.to_string());
                node.children.push(unk);
            }
        }
    }

    node
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();

    let password = match args.password {
        Some(p) => p,
        None => match Password::new().with_prompt("Password").interact() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to read password: {}", e);
                std::process::exit(1);
            }
        },
    };

    eprintln!("Connecting to {}...", args.address);
    let disconnected = match mikrotik_api::connect(&args.address).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to {}: {}", args.address, e);
            std::process::exit(1);
        }
    };

    eprintln!("Authenticating as '{}'...", args.login);
    let mut api = match disconnected.authenticate(&args.login, &password).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Authentication failed: {}", e);
            std::process::exit(1);
        }
    };

    let start_path = args.path.unwrap_or_default();
    let root_name = if start_path.is_empty() {
        "root".to_string()
    } else {
        start_path.clone()
    };

    let include_args = !args.no_args;
    let include_docs = args.include_docs;

    eprintln!(
        "Extracting command tree from '{}' (include_args={}, include_docs={})...",
        root_name, include_args, include_docs
    );

    let config = ExtractConfig {
        max_depth: args.max_depth,
        include_args,
        include_docs,
    };

    let start_time = Instant::now();
    let mut visited = HashSet::new();
    let tree = extract_subtree(
        &mut api,
        &start_path,
        &root_name,
        "root",
        0,
        config,
        &mut visited,
    )
    .await;
    let elapsed = start_time.elapsed();

    let (dirs, cmds, args_count) = tree.count_stats();
    eprintln!(
        "Extracted {} menus/dirs, {} commands, and {} arguments in {:.2}s.",
        dirs,
        cmds,
        args_count,
        elapsed.as_secs_f64()
    );

    let output_str = match args.format {
        OutputFormat::Tree => {
            let mut out = String::new();
            tree.print_tree("", true, true, &mut out);
            out
        }
        OutputFormat::Json => match serde_json::to_string_pretty(&tree) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Failed to serialize tree to JSON: {}", e);
                std::process::exit(1);
            }
        },
        OutputFormat::Summary => {
            format!(
                "Command Tree Summary:\n  Root Path: {}\n  Menus/Directories: {}\n  Commands: {}\n  Arguments: {}\n  Extraction Time: {:.2}s\n",
                root_name, dirs, cmds, args_count, elapsed.as_secs_f64()
            )
        }
    };

    if let Some(path) = args.output {
        if let Err(e) = std::fs::write(&path, &output_str) {
            eprintln!("Failed to write to '{}': {}", path.display(), e);
            std::process::exit(1);
        }
        eprintln!("Output saved to '{}'.", path.display());
    } else {
        print!("{}", output_str);
        let _ = std::io::stdout().flush();
    }
}
