use std::collections::HashMap;

use super::{SafetyClass, UncheckedCommand};

/// Outcome of classifying a command before policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// The judged safety quadrant.
    pub class: SafetyClass,
    /// Optional human-readable rationale when classified as dangerous or needing special care.
    pub reason: Option<String>,
}

/// Black-box classifier trait judging unverified RouterOS commands.
pub trait Classifier: Send + Sync {
    /// Classifies an unverified command into one of the four safety quadrants.
    fn classify(&self, cmd: &UncheckedCommand) -> Classification;
}

/// Deterministic rule-based classifier enforcing fail-closed boundaries.
#[derive(Debug, Default, Clone)]
pub struct RuleClassifier;

impl Classifier for RuleClassifier {
    fn classify(&self, cmd: &UncheckedCommand) -> Classification {
        let path = cmd.path.as_str();
        let verb = path.rsplit('/').next().unwrap_or("");

        // 1. DangerousWrite check: high blast-radius or lockout-prone mutations
        if let Some(reason) = check_dangerous_write(path, &cmd.attributes) {
            return Classification {
                class: SafetyClass::DangerousWrite,
                reason: Some(reason),
            };
        }

        // 2. DangerousRead check: secrets leakage, private keys, or unbounded load
        if let Some(reason) = check_dangerous_read(path, &cmd.attributes) {
            return Classification {
                class: SafetyClass::DangerousRead,
                reason: Some(reason),
            };
        }

        // 3. Verb classification
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
            // Fail-closed on unrecognized or custom command verbs
            _ => Classification {
                class: SafetyClass::DangerousWrite,
                reason: Some(format!(
                    "Unrecognized or privileged command verb '{verb}' (fail-closed)"
                )),
            },
        }
    }
}

fn check_dangerous_write(path: &str, attrs: &HashMap<String, String>) -> Option<String> {
    // Catastrophic system commands
    if path == "/system/reset-configuration"
        || path.starts_with("/system/reset-configuration/")
        || path == "/system/reboot"
        || path == "/system/shutdown"
    {
        return Some(format!("Catastrophic system operation '{path}'"));
    }

    // Disk formatting or filesystem partition manipulation
    if path.starts_with("/disk/format")
        || path.starts_with("/system/package/uninstall")
        || path.starts_with("/system/routerboard/upgrade")
    {
        return Some(format!("Storage or firmware mutation '{path}'"));
    }

    // User account or permission modifications (prevents lockout or privilege escalation)
    if path.starts_with("/user/") && !path.ends_with("/print") && !path.ends_with("/get") {
        return Some("Modification of user accounts or group policies is restricted".into());
    }

    // Service modifications (disabling API or SSH services severs remote control)
    if path.starts_with("/ip/service/") {
        let verb = path.rsplit('/').next().unwrap_or("");
        if matches!(verb, "disable" | "set" | "remove") {
            return Some("Modifying IP services could sever API or remote access".into());
        }
    }

    // Disabling critical interfaces (e.g. disabling ether1 or bridge)
    if path.starts_with("/interface/") {
        let verb = path.rsplit('/').next().unwrap_or("");
        if verb == "disable" || verb == "remove" {
            if let Some(numbers) = attrs.get("numbers").or_else(|| attrs.get(".id")) {
                if numbers == "*1" || numbers == "0" || numbers.contains("ether1") {
                    return Some("Disabling primary/default interface is prohibited".into());
                }
            }
        }
    }

    None
}

fn check_dangerous_read(path: &str, attrs: &HashMap<String, String>) -> Option<String> {
    // Exporting sensitive credentials, keys, or passwords
    if path == "/export" || path.starts_with("/export/") {
        if let Some(val) = attrs.get("show-sensitive").or_else(|| attrs.get("verbose")) {
            if val == "yes" || val == "true" {
                return Some("Exporting sensitive configuration secrets is blocked".into());
            }
        }
    }

    // Private SSH keys or TLS private keys
    if path.contains("/user/ssh-keys/private")
        || path.contains("/certificate/export-certificate")
        || path.contains("/ip/ipsec/key")
    {
        return Some("Retrieving private keys or certificate secrets is blocked".into());
    }

    // Unbounded bandwidth test (can flood network and starve traffic)
    if (path == "/tool/bandwidth-test" || path.starts_with("/tool/bandwidth-test/"))
        && !attrs.contains_key("duration")
    {
        return Some(
            "/tool/bandwidth-test requires an explicit 'duration' attribute to prevent saturation"
                .into(),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_read_commands() {
        let classifier = RuleClassifier;

        let cmd = UncheckedCommand::new("/interface/print");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeRead);

        let cmd = UncheckedCommand::new("/ip/address/print");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeRead);

        let cmd = UncheckedCommand::new("/console/inspect");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeRead);

        let cmd = UncheckedCommand::new("/ping");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeRead);

        let cmd = UncheckedCommand::new("/log/listen");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeRead);
    }

    #[test]
    fn test_dangerous_read_commands() {
        let classifier = RuleClassifier;

        let mut cmd = UncheckedCommand::new("/export");
        cmd.attributes
            .insert("show-sensitive".to_string(), "yes".to_string());
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousRead);

        let cmd = UncheckedCommand::new("/user/ssh-keys/private/print");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousRead);

        let cmd = UncheckedCommand::new("/tool/bandwidth-test");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousRead);
    }

    #[test]
    fn test_safe_write_commands() {
        let classifier = RuleClassifier;

        let cmd = UncheckedCommand::new("/ip/dhcp-server/lease/add");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeWrite);

        let cmd = UncheckedCommand::new("/ip/firewall/filter/set");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeWrite);

        let cmd = UncheckedCommand::new("/interface/ethernet/set");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::SafeWrite);
    }

    #[test]
    fn test_dangerous_write_commands() {
        let classifier = RuleClassifier;

        let cmd = UncheckedCommand::new("/system/reset-configuration");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousWrite);

        let cmd = UncheckedCommand::new("/system/reboot");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousWrite);

        let cmd = UncheckedCommand::new("/user/remove");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousWrite);

        let cmd = UncheckedCommand::new("/ip/service/disable");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousWrite);

        let cmd = UncheckedCommand::new("/system/arbitrary-unknown-verb");
        assert_eq!(classifier.classify(&cmd).class, SafetyClass::DangerousWrite);
    }
}
