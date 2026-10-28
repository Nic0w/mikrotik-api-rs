pub mod classifier;

use std::collections::HashMap;

use clap::ValueEnum;
pub use classifier::{Classification, Classifier, RuleClassifier};
use serde::Serialize;

/// The four safety quadrants every command is categorized into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyClass {
    /// Safe read-only operations (e.g. /interface/print, /console/inspect).
    SafeRead,
    /// High-risk read operations (e.g. /export show-sensitive=yes, private keys).
    DangerousRead,
    /// Standard mutations permitted under write policies (e.g. adding static DHCP lease).
    SafeWrite,
    /// Critical or catastrophic mutations (e.g. /system/reset-configuration, /system/reboot).
    DangerousWrite,
}

/// Server policy enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum PolicyMode {
    /// Only SafeRead operations permitted (Default).
    #[default]
    ReadOnly,
    /// SafeRead permitted; SafeWrite requires approval; Dangerous commands denied.
    ConfirmWrites,
    /// SafeRead and SafeWrite permitted; Dangerous commands denied.
    ReadWrite,
}

/// Unverified command assembled from MCP tool arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncheckedCommand {
    /// Normalized command path (e.g. "/ip/address/print").
    pub path: String,
    /// Key-value command attributes.
    pub attributes: HashMap<String, String>,
    /// Query filters without leading '?' (e.g. "running=true").
    pub query: Vec<String>,
    /// Selected properties to return (e.g. ["name", "mtu"]).
    pub proplist: Vec<String>,
    /// Whether this is a streaming command (e.g. /log/listen).
    pub is_streaming: bool,
}

impl UncheckedCommand {
    /// Creates a new `UncheckedCommand` normalizing comma-separated, missing-slash,
    /// or CLI-style space-separated paths and extracting any inline attributes or queries.
    pub fn new(raw_path: &str) -> Self {
        let (normalized, attributes, query, proplist) = parse_command_input(raw_path);
        let is_streaming = normalized.ends_with("/listen");
        Self {
            path: normalized,
            attributes,
            query,
            proplist,
            is_streaming,
        }
    }
}

/// Capability token proving a command has passed `SafetyGate::authorize`.
/// The inner fields are private to `safety` so outside modules cannot instantiate it directly.
#[derive(Debug, Clone)]
pub struct AuthorizedCommand {
    inner: UncheckedCommand,
    class: SafetyClass,
}

impl AuthorizedCommand {
    /// Returns the normalized command path.
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    /// Returns the verified safety class.
    pub fn class(&self) -> SafetyClass {
        self.class
    }

    /// Whether this command is a streaming listener.
    pub fn is_streaming(&self) -> bool {
        self.inner.is_streaming
    }

    /// Converts command properties, query filters, and attributes into wire tuples.
    pub fn to_wire_attributes(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if !self.inner.proplist.is_empty() {
            out.push(("=.proplist".to_string(), self.inner.proplist.join(",")));
        }
        for q in &self.inner.query {
            let key = if q.starts_with('?') {
                q.clone()
            } else {
                format!("?{q}")
            };
            out.push((key, String::new()));
        }
        for (k, v) in &self.inner.attributes {
            out.push((k.clone(), v.clone()));
        }
        out
    }
}

/// Reason why a command was not authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyStop {
    /// Command is strictly rejected.
    Denied {
        /// Classification assigned by the safety layer.
        class: SafetyClass,
        /// Explanation for the rejection.
        reason: String,
    },
    /// Command requires explicit user approval before execution.
    NeedsConfirmation {
        /// Classification assigned by the safety layer.
        class: SafetyClass,
        /// Explanation why confirmation is required.
        reason: String,
        /// The original unchecked command to be executed upon confirmation.
        pending: Box<UncheckedCommand>,
    },
}

/// Gatekeeper applying server policy to classified commands.
pub struct SafetyGate<C = RuleClassifier> {
    mode: PolicyMode,
    classifier: C,
}

impl<C: Classifier> SafetyGate<C> {
    /// Creates a new `SafetyGate` with the given mode and classifier.
    pub fn new(mode: PolicyMode, classifier: C) -> Self {
        Self { mode, classifier }
    }

    /// Evaluates an `UncheckedCommand` and returns an `AuthorizedCommand` if approved.
    pub fn authorize(&self, cmd: UncheckedCommand) -> Result<AuthorizedCommand, PolicyStop> {
        let report = self.classifier.classify(&cmd);
        let class = report.class;

        match (self.mode, class) {
            (_, SafetyClass::SafeRead) => Ok(AuthorizedCommand { inner: cmd, class }),

            (PolicyMode::ReadWrite, SafetyClass::SafeWrite) => {
                Ok(AuthorizedCommand { inner: cmd, class })
            }

            (PolicyMode::ConfirmWrites, SafetyClass::SafeWrite) => {
                Err(PolicyStop::NeedsConfirmation {
                    class,
                    reason: report.reason.unwrap_or_else(|| {
                        format!("Write command '{}' requires user confirmation", cmd.path)
                    }),
                    pending: Box::new(cmd),
                })
            }

            (PolicyMode::ReadOnly, SafetyClass::SafeWrite) => Err(PolicyStop::Denied {
                class,
                reason: format!(
                    "Command '{}' is a write operation, but the server is running in read-only mode",
                    cmd.path
                ),
            }),

            (_, SafetyClass::DangerousRead | SafetyClass::DangerousWrite) => {
                Err(PolicyStop::Denied {
                    class,
                    reason: report.reason.unwrap_or_else(|| {
                        format!("Command '{}' is blocked by safety policy ({:?})", cmd.path, class)
                    }),
                })
            }
        }
    }
}

/// Parses raw command input that may contain CLI-style spaces, commas, or inline parameters,
/// producing a canonical RouterOS path and extracting any inline attributes, query filters, or proplists.
pub fn parse_command_input(
    raw: &str,
) -> (String, HashMap<String, String>, Vec<String>, Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ("/".to_string(), HashMap::new(), Vec::new(), Vec::new());
    }

    let mut path_tokens = Vec::new();
    let mut attributes = HashMap::new();
    let mut query = Vec::new();
    let mut proplist = Vec::new();

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    for token in tokens {
        // Query filter: ?key=val or ?#|
        if let Some(q) = token.strip_prefix('?') {
            query.push(q.to_string());
        }
        // Inline proplist: .proplist=name,type or =.proplist=name,type
        else if let Some(stripped) = token
            .strip_prefix(".proplist=")
            .or_else(|| token.strip_prefix("=.proplist="))
        {
            for p in stripped.split(',') {
                let p_trim = p.trim();
                if !p_trim.is_empty() {
                    proplist.push(p_trim.to_string());
                }
            }
        }
        // Inline attributes: key=val or =key=val (not starting with /)
        else if token.contains('=') && !token.starts_with('/') {
            let clean = token.trim_start_matches('=');
            if let Some((k, v)) = clean.split_once('=') {
                attributes.insert(k.to_string(), v.to_string());
            }
        }
        // CLI display modifiers or "where" following "print"
        else if path_tokens.last().copied() == Some("print")
            && (matches!(
                token.to_ascii_lowercase().as_str(),
                "detail" | "brief" | "terse" | "without-paging"
            ) || token.eq_ignore_ascii_case("where"))
        {
            continue;
        }
        // Path segments (may contain slashes, commas)
        else {
            for part in token.split(['/', ',']) {
                let p = part.trim();
                if !p.is_empty() {
                    path_tokens.push(p);
                }
            }
        }
    }

    let normalized_path = if path_tokens.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path_tokens.join("/"))
    };

    (normalized_path, attributes, query, proplist)
}

/// Normalizes raw command paths like "ip,address,print", "/ip/address print detail",
/// or "/ip dhcp-server print" to canonical RouterOS API paths like "/ip/address/print".
pub fn normalize_command_path(raw: &str) -> String {
    parse_command_input(raw).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_normalization() {
        assert_eq!(
            normalize_command_path("ip,address,print"),
            "/ip/address/print"
        );
        assert_eq!(
            normalize_command_path("ip/address/print"),
            "/ip/address/print"
        );
        assert_eq!(
            normalize_command_path("/ip/address/print"),
            "/ip/address/print"
        );
        assert_eq!(
            normalize_command_path("  /system/resource/print  "),
            "/system/resource/print"
        );
        assert_eq!(normalize_command_path(""), "/");

        // CLI space normalization and modifier stripping
        assert_eq!(
            normalize_command_path("/ip/address print detail"),
            "/ip/address/print"
        );
        assert_eq!(
            normalize_command_path("/ip/dhcp-server print"),
            "/ip/dhcp-server/print"
        );
        assert_eq!(
            normalize_command_path("/ip dhcp-server print"),
            "/ip/dhcp-server/print"
        );
        assert_eq!(
            normalize_command_path("ip address print"),
            "/ip/address/print"
        );
        assert_eq!(
            normalize_command_path("/interface print terse without-paging"),
            "/interface/print"
        );
    }

    #[test]
    fn test_parse_command_input_inline_attrs_and_queries() {
        let cmd =
            UncheckedCommand::new("/ip/firewall/filter print where chain=forward ?disabled=false");
        assert_eq!(cmd.path, "/ip/firewall/filter/print");
        assert_eq!(cmd.attributes.get("chain"), Some(&"forward".to_string()));
        assert_eq!(cmd.query, vec!["disabled=false".to_string()]);

        let cmd2 = UncheckedCommand::new("/interface print .proplist=name,type,running");
        assert_eq!(cmd2.path, "/interface/print");
        assert_eq!(cmd2.proplist, vec!["name", "type", "running"]);
    }

    #[test]
    fn test_wire_attributes_formatting() {
        let mut cmd = UncheckedCommand::new("/interface/print");
        cmd.proplist = vec!["name".into(), "mtu".into()];
        cmd.query = vec!["running=true".into(), "?disabled=false".into()];
        cmd.attributes.insert("detail".into(), "".into());

        let gate = SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier);
        let auth = gate.authorize(cmd).expect("authorized");

        let wire = auth.to_wire_attributes();
        assert_eq!(wire[0], ("=.proplist".to_string(), "name,mtu".to_string()));
        assert_eq!(wire[1], ("?running=true".to_string(), "".to_string()));
        assert_eq!(wire[2], ("?disabled=false".to_string(), "".to_string()));
        assert_eq!(wire[3], ("detail".to_string(), "".to_string()));
    }

    #[test]
    fn test_policy_gate_read_only_matrix() {
        let gate = SafetyGate::new(PolicyMode::ReadOnly, RuleClassifier);

        // SafeRead -> Ok
        let res = gate.authorize(UncheckedCommand::new("/interface/print"));
        assert!(res.is_ok());

        // SafeWrite -> Denied
        let res = gate.authorize(UncheckedCommand::new("/ip/dhcp-server/lease/add"));
        assert!(matches!(
            res,
            Err(PolicyStop::Denied {
                class: SafetyClass::SafeWrite,
                ..
            })
        ));

        // DangerousRead -> Denied
        let mut export_cmd = UncheckedCommand::new("/export");
        export_cmd
            .attributes
            .insert("show-sensitive".into(), "yes".into());
        let res = gate.authorize(export_cmd);
        assert!(matches!(
            res,
            Err(PolicyStop::Denied {
                class: SafetyClass::DangerousRead,
                ..
            })
        ));

        // DangerousWrite -> Denied
        let res = gate.authorize(UncheckedCommand::new("/system/reset-configuration"));
        assert!(matches!(
            res,
            Err(PolicyStop::Denied {
                class: SafetyClass::DangerousWrite,
                ..
            })
        ));
    }

    #[test]
    fn test_policy_gate_confirm_writes_matrix() {
        let gate = SafetyGate::new(PolicyMode::ConfirmWrites, RuleClassifier);

        // SafeRead -> Ok
        let res = gate.authorize(UncheckedCommand::new("/interface/print"));
        assert!(res.is_ok());

        // SafeWrite -> NeedsConfirmation
        let res = gate.authorize(UncheckedCommand::new("/ip/dhcp-server/lease/add"));
        assert!(matches!(
            res,
            Err(PolicyStop::NeedsConfirmation {
                class: SafetyClass::SafeWrite,
                ..
            })
        ));

        // DangerousWrite -> Denied
        let res = gate.authorize(UncheckedCommand::new("/system/reboot"));
        assert!(matches!(
            res,
            Err(PolicyStop::Denied {
                class: SafetyClass::DangerousWrite,
                ..
            })
        ));
    }

    #[test]
    fn test_policy_gate_read_write_matrix() {
        let gate = SafetyGate::new(PolicyMode::ReadWrite, RuleClassifier);

        // SafeRead -> Ok
        let res = gate.authorize(UncheckedCommand::new("/interface/print"));
        assert!(res.is_ok());

        // SafeWrite -> Ok
        let res = gate.authorize(UncheckedCommand::new("/ip/dhcp-server/lease/add"));
        assert!(res.is_ok());

        // DangerousWrite -> Denied
        let res = gate.authorize(UncheckedCommand::new("/system/reboot"));
        assert!(matches!(
            res,
            Err(PolicyStop::Denied {
                class: SafetyClass::DangerousWrite,
                ..
            })
        ));
    }
}
