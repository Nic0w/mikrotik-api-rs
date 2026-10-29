pub mod classifier;
pub mod grammar;

use std::collections::HashMap;

use clap::ValueEnum;
pub use classifier::{Classification, Classifier, RuleClassifier};
pub use grammar::{parse_command_line, ParsedCommand};
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
    /// Optional grammar/structural error detected during command parsing.
    pub parse_error: Option<String>,
}

impl UncheckedCommand {
    /// Creates a new `UncheckedCommand` using the formal RouterOS EBNF grammar parser,
    /// normalizing paths and extracting any inline attributes, quoted strings, or queries.
    pub fn new(raw_path: &str) -> Self {
        let parsed = parse_command_line(raw_path);
        let is_streaming = parsed.path.ends_with("/listen");
        Self {
            path: parsed.path,
            attributes: parsed.attributes,
            query: parsed.query,
            proplist: parsed.proplist,
            is_streaming,
            parse_error: parsed.parse_error,
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

/// Normalizes raw command paths like "ip,address,print", "/ip/address print detail",
/// or "/ip dhcp-server print" to canonical RouterOS API paths like "/ip/address/print".
pub fn normalize_command_path(raw: &str) -> String {
    parse_command_line(raw).path
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
        assert_eq!(
            cmd.query,
            vec!["chain=forward".to_string(), "disabled=false".to_string()]
        );

        let cmd2 = UncheckedCommand::new("/interface print .proplist=name,type,running");
        assert_eq!(cmd2.path, "/interface/print");
        assert_eq!(cmd2.proplist, vec!["name", "type", "running"]);

        let cmd3 = UncheckedCommand::new(
            r#"/ip/firewall/filter/add action=accept chain=input dst-port=1337 protocol=tcp connection-state=new comment="Allow incoming IPv4 traffic on port 1337" log=yes priority=1"#,
        );
        assert_eq!(cmd3.path, "/ip/firewall/filter/add");
        assert_eq!(
            cmd3.attributes.get("comment").map(String::as_str),
            Some("Allow incoming IPv4 traffic on port 1337")
        );
        assert_eq!(cmd3.attributes.get("log").map(String::as_str), Some("yes"));
        assert_eq!(
            cmd3.attributes.get("priority").map(String::as_str),
            Some("1")
        );
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
