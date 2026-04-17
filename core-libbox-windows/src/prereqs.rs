#![cfg_attr(libbox_stub, allow(dead_code, unused_imports))]

use domain::{PrerequisiteCheck, VpnError};
use pingle_netwatch::{NetwatcherBackend, Watcher};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIBBOX_DLL_CHECK: &str = "libbox.dll";
const ADMIN_RIGHTS_CHECK: &str = "admin_rights";
const FIREWALL_RULES_CHECK: &str = "firewall_rules";
const NETWATCH_CHECK: &str = "netwatch";
const FIREWALL_RULE_INBOUND: &str = "Pingle Daemon (Inbound)";
const FIREWALL_RULE_OUTBOUND: &str = "Pingle Daemon (Outbound)";

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirewallRuleProbe {
    inbound_ok: bool,
    outbound_ok: bool,
    firewall_enabled: bool,
    executable_path: Option<PathBuf>,
}

pub fn runtime_available(checks: &[PrerequisiteCheck]) -> bool {
    checks
        .iter()
        .find(|check| check.name == LIBBOX_DLL_CHECK)
        .map(|check| check.passed)
        .unwrap_or(false)
}

pub fn collect_prerequisites() -> Vec<PrerequisiteCheck> {
    let mut checks = vec![PrerequisiteCheck {
        name: LIBBOX_DLL_CHECK.into(),
        passed: true,
        message: "linked".into(),
    }];

    let admin = match is_running_as_admin() {
        Ok(true) => PrerequisiteCheck {
            name: ADMIN_RIGHTS_CHECK.into(),
            passed: true,
            message: "running elevated".into(),
        },
        Ok(false) => PrerequisiteCheck {
            name: ADMIN_RIGHTS_CHECK.into(),
            passed: false,
            message: "Windows libbox connect path requires Administrator rights".into(),
        },
        Err(error) => PrerequisiteCheck {
            name: ADMIN_RIGHTS_CHECK.into(),
            passed: false,
            message: error,
        },
    };
    checks.push(admin);

    let firewall = match probe_firewall_rules_for_current_exe() {
        Ok(probe) => firewall_probe_to_check(&probe),
        Err(error) => PrerequisiteCheck {
            name: FIREWALL_RULES_CHECK.into(),
            passed: false,
            message: error,
        },
    };
    checks.push(firewall);

    let netwatch = match NetwatcherBackend::new().list_interfaces() {
        Ok(interfaces) => PrerequisiteCheck {
            name: NETWATCH_CHECK.into(),
            passed: true,
            message: format!("interface watcher available ({} interfaces visible)", interfaces.len()),
        },
        Err(error) => PrerequisiteCheck {
            name: NETWATCH_CHECK.into(),
            passed: false,
            message: format!("interface watcher unavailable: {error}"),
        },
    };
    checks.push(netwatch);

    checks
}

pub fn ensure_firewall_rules_for_current_exe() -> Result<(), VpnError> {
    #[cfg(not(windows))]
    {
        Ok(())
    }

    #[cfg(windows)]
    {
        let exe = current_executable_path()
            .map_err(|error| VpnError::PermissionDenied(format!("resolve current executable: {error}")))?;
        ensure_firewall_rules(&exe)
            .map_err(|error| VpnError::PermissionDenied(format!("ensure Windows Firewall rules: {error}")))
    }
}

fn firewall_probe_to_check(probe: &FirewallRuleProbe) -> PrerequisiteCheck {
    let name = FIREWALL_RULES_CHECK.into();

    if !probe.firewall_enabled {
        return PrerequisiteCheck {
            name,
            passed: true,
            message: "Windows Firewall is disabled; host rules not required".into(),
        };
    }

    let Some(path) = probe.executable_path.as_ref() else {
        return PrerequisiteCheck {
            name,
            passed: false,
            message: "could not resolve daemon executable path for firewall rule checks".into(),
        };
    };

    if probe.inbound_ok && probe.outbound_ok {
        return PrerequisiteCheck {
            name,
            passed: true,
            message: format!("inbound/outbound allow rules present for {}", path.display()),
        };
    }

    let mut missing = Vec::new();
    if !probe.inbound_ok {
        missing.push("inbound");
    }
    if !probe.outbound_ok {
        missing.push("outbound");
    }

    PrerequisiteCheck {
        name,
        passed: false,
        message: format!(
            "missing {} Windows Firewall rule(s) for {}",
            missing.join("+"),
            path.display()
        ),
    }
}

#[cfg(windows)]
fn current_executable_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))
}

#[cfg(not(windows))]
fn current_executable_path() -> Result<PathBuf, String> {
    Err("Windows firewall checks are unavailable on this host".into())
}

#[cfg(windows)]
fn is_running_as_admin() -> Result<bool, String> {
    let output = Command::new("net")
        .arg("session")
        .output()
        .map_err(|error| format!("run `net session`: {error}"))?;
    Ok(output.status.success())
}

#[cfg(not(windows))]
fn is_running_as_admin() -> Result<bool, String> {
    Err("admin-rights probe is only available on Windows".into())
}

#[cfg(windows)]
fn probe_firewall_rules_for_current_exe() -> Result<FirewallRuleProbe, String> {
    let exe = current_executable_path()?;
    probe_firewall_rules_for_executable(&exe)
}

#[cfg(not(windows))]
fn probe_firewall_rules_for_current_exe() -> Result<FirewallRuleProbe, String> {
    Err("Windows Firewall probe is only available on Windows".into())
}

#[cfg(windows)]
fn ensure_firewall_rules(executable: &Path) -> Result<(), String> {
    for (rule_name, direction) in [
        (FIREWALL_RULE_INBOUND, "in"),
        (FIREWALL_RULE_OUTBOUND, "out"),
    ] {
        let _ = Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                &format!("name={rule_name}"),
            ])
            .output();

        let output = Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={rule_name}"),
                &format!("dir={direction}"),
                "action=allow",
                &format!("program={}", executable.display()),
                "enable=yes",
                "profile=any",
            ])
            .output()
            .map_err(|error| format!("run `netsh advfirewall firewall add rule` for {rule_name}: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "{rule_name}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn probe_firewall_rules_for_executable(executable: &Path) -> Result<FirewallRuleProbe, String> {
    let firewall_enabled = firewall_enabled()?;
    let inbound_ok = firewall_rule_matches(FIREWALL_RULE_INBOUND, executable, "in")?;
    let outbound_ok = firewall_rule_matches(FIREWALL_RULE_OUTBOUND, executable, "out")?;
    Ok(FirewallRuleProbe {
        inbound_ok,
        outbound_ok,
        firewall_enabled,
        executable_path: Some(executable.to_path_buf()),
    })
}

#[cfg(windows)]
fn firewall_enabled() -> Result<bool, String> {
    let output = Command::new("netsh")
        .args(["advfirewall", "show", "currentprofile"])
        .output()
        .map_err(|error| format!("run `netsh advfirewall show currentprofile`: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let lower = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    Ok(lower.contains("state") && lower.contains("on"))
}

#[cfg(windows)]
fn firewall_rule_matches(rule_name: &str, executable: &Path, direction: &str) -> Result<bool, String> {
    let output = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={rule_name}"),
            "verbose",
        ])
        .output()
        .map_err(|error| format!("run `netsh advfirewall firewall show rule` for {rule_name}: {error}"))?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(parse_firewall_rule_output(
        &String::from_utf8_lossy(&output.stdout),
        executable,
        direction,
    ))
}

fn parse_firewall_rule_output(output: &str, executable: &Path, direction: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    if lower.contains("no rules match the specified criteria") {
        return false;
    }

    let executable = executable
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let direction_token = if direction.eq_ignore_ascii_case("in") {
        "in"
    } else {
        "out"
    };

    lower.contains("enabled:")
        && lower.contains("yes")
        && lower.contains("direction:")
        && lower.contains(direction_token)
        && lower.contains("program:")
        && lower.contains(&executable)
}

#[cfg(test)]
mod tests {
    use super::{parse_firewall_rule_output, runtime_available, PrerequisiteCheck};
    use std::path::Path;

    #[test]
    fn runtime_available_only_requires_linked_libbox() {
        let checks = vec![
            PrerequisiteCheck {
                name: "libbox.dll".into(),
                passed: true,
                message: "linked".into(),
            },
            PrerequisiteCheck {
                name: "admin_rights".into(),
                passed: false,
                message: "not elevated".into(),
            },
        ];

        assert!(runtime_available(&checks));
    }

    #[test]
    fn firewall_rule_parser_accepts_matching_enabled_rule() {
        let sample = r#"
Rule Name:                            Pingle Daemon (Inbound)
Enabled:                              Yes
Direction:                            In
Program:                              C:\Program Files\Pingle\ipc-server-headless.exe
"#;

        assert!(parse_firewall_rule_output(
            sample,
            Path::new(r"C:\Program Files\Pingle\ipc-server-headless.exe"),
            "in"
        ));
    }

    #[test]
    fn firewall_rule_parser_rejects_wrong_program_or_disabled_rule() {
        let sample = r#"
Rule Name:                            Pingle Daemon (Outbound)
Enabled:                              No
Direction:                            Out
Program:                              C:\Elsewhere\daemon.exe
"#;

        assert!(!parse_firewall_rule_output(
            sample,
            Path::new(r"C:\Program Files\Pingle\ipc-server-headless.exe"),
            "out"
        ));
    }
}
