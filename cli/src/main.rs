//! Pingle CLI — headless binary for scripting, CI, and debugging.
//!
//! Provides a uniform `clap`-based interface to any VPN core engine without
//! starting the Tauri daemon or system tray. Useful for:
//! - Scripted connection management in CI/CD or shell scripts
//! - Debugging config loading and core validation independent of the Flutter UI
//! - Testing that sing-box starts correctly before launching the full daemon
//!
//! Wires: `ConfigLoader` → `CoreRegistry` → `VpnManager` → subcommand dispatch.

use app_config::{ConfigLoader, PinglingConfig};
use clap::{Parser, Subcommand};
use data::MemorySettingsStorage;
use domain::{CoreDescriptor, CoreSource, SettingsStorage};
use service::{CoreRegistry, VpnManager};
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(
    name = "pingling",
    about = "VPN core daemon — uniform CLI for any VPN engine",
    version,
    long_about = "Wraps VPN core engines (sing-box, xray, ...) into a uniform CLI.\n\
                  Start, stop, check status, validate configs, and query info."
)]
struct Cli {
    /// Path to pingling config file (YAML or JSON).
    /// If omitted, searches default locations then falls back to env vars.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// VPN core config file path (passed to the core engine).
    /// Overrides core_config_path from the config file.
    #[arg(short = 'C', long, global = true)]
    vpn_config: Option<String>,

    /// Override the core type (e.g. "sing-box", "mock").
    /// Overrides core_type from the config file.
    #[arg(short = 't', long, global = true)]
    core_type: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the VPN core engine.
    Start,

    /// Gracefully stop the VPN core engine.
    Stop,

    /// Force-kill the VPN core engine.
    Kill,

    /// Restart the VPN core engine (stop then start).
    Restart,

    /// Show current connection status.
    Status,

    /// Show core engine metadata.
    Info,

    /// Validate a VPN core configuration file.
    Validate {
        /// Path to the config file to validate.
        /// If omitted, uses core_config_path from config.
        config_path: Option<String>,
    },

    /// Run prerequisite checks.
    Prereqs,

    /// List available VPN core engines.
    Cores,
}

fn main() {
    let cli = Cli::parse();

    // -- Load config ----------------------------------------------------------

    let mut cfg = match &cli.config {
        Some(path) => ConfigLoader::from_file(path),
        None => ConfigLoader::from_env(),
    }
    .unwrap_or_else(|e| {
        eprintln!("error: failed to load config: {e}");
        process::exit(1);
    });

    // CLI --vpn-config overrides config file value
    if let Some(vpn_config) = &cli.vpn_config {
        cfg.core_config_path = vpn_config.clone();
    }

    // CLI --core-type overrides config file value
    if let Some(core_type) = &cli.core_type {
        cfg.core_type = core_type.clone();
    }

    // -- Logger ---------------------------------------------------------------

    let level_filter = match cfg.log_level.as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    };

    env_logger::Builder::new()
        .filter_module("pingling", level_filter)
        .format_target(false)
        .format_timestamp(None)
        .init();

    // -- Build registry + manager ---------------------------------------------

    let registry = build_registry(&cfg);
    let mut storage = MemorySettingsStorage::new();
    if !cfg.core_config_path.is_empty() {
        if let Err(e) = storage.set_string("config_path", &cfg.core_config_path) {
            eprintln!("error: failed to set config_path: {e}");
            process::exit(1);
        }
    }
    let mgr = VpnManager::new(registry, Box::new(storage));

    // -- Dispatch --------------------------------------------------------------

    let exit_code = match &cli.command {
        Commands::Cores => cmd_cores(&mgr),
        Commands::Start => cmd_start(&mgr, &cfg),
        Commands::Stop => cmd_stop(&mgr),
        Commands::Kill => cmd_kill(&mgr),
        Commands::Restart => cmd_restart(&mgr, &cfg),
        Commands::Status => cmd_status(&mgr),
        Commands::Info => cmd_info(&mgr),
        Commands::Validate { config_path } => cmd_validate(&mgr, config_path, &cfg),
        Commands::Prereqs => cmd_prereqs(&mgr),
    };

    process::exit(exit_code);
}

/// Build a CoreRegistry with all available cores.
fn build_registry(cfg: &PinglingConfig) -> CoreRegistry {
    let mut registry = CoreRegistry::new();

    // 1. Register the configured core type
    match cfg.core_type.as_str() {
        "sing-box" => {
            let binary_path = if cfg.core_binary_path.is_empty() {
                util::which("sing-box").unwrap_or_default()
            } else {
                cfg.core_binary_path.clone()
            };
            let available = !binary_path.is_empty() && std::path::Path::new(&binary_path).exists();

            let core = core_singbox_standalone::SingboxStandalone::new(&binary_path);
            registry.register(
                CoreDescriptor {
                    core_type: "sing-box".into(),
                    display_name: "Sing-Box".into(),
                    source: if cfg.core_binary_path.is_empty() {
                        CoreSource::System
                    } else {
                        CoreSource::Linked(cfg.core_binary_path.clone())
                    },
                    binary_path: if binary_path.is_empty() {
                        None
                    } else {
                        Some(binary_path)
                    },
                    available,
                },
                Box::new(core),
            );
        }
        #[cfg(feature = "mock")]
        "mock" => {
            registry.register(
                CoreDescriptor {
                    core_type: "mock".into(),
                    display_name: "Mock (Debug)".into(),
                    source: CoreSource::Mocked,
                    binary_path: None,
                    available: true,
                },
                Box::new(core_mock::MockCore::new()),
            );
        }
        other => {
            eprintln!("error: unknown core type: {other}");
            eprintln!(
                "supported: sing-box{}",
                if cfg!(feature = "mock") { ", mock" } else { "" }
            );
            process::exit(1);
        }
    }

    // 2. Discover additional system cores
    registry.discover();

    registry
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_cores(mgr: &VpnManager) -> i32 {
    let cores = mgr.list_cores();
    let active = mgr.active_core_type();

    if cores.is_empty() {
        println!("no cores discovered");
        return 0;
    }

    println!("{:<12} {:<20} {:<10} PATH", "TYPE", "NAME", "SOURCE");
    println!("{}", "-".repeat(70));

    for core in &cores {
        let marker = if active.as_deref() == Some(&core.core_type) {
            "*"
        } else {
            " "
        };
        let path = core.binary_path.as_deref().unwrap_or("-");
        let avail = if core.available { "" } else { " [not found]" };
        println!(
            "{marker} {:<11} {:<20} {:<10} {}{avail}",
            core.core_type, core.display_name, core.source, path
        );
    }

    println!("\n  * = active core");
    0
}

fn cmd_start(mgr: &VpnManager, cfg: &PinglingConfig) -> i32 {
    match mgr.connect() {
        Ok(()) => {
            println!("started ({})", cfg.core_type);
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_stop(mgr: &VpnManager) -> i32 {
    match mgr.disconnect() {
        Ok(()) => {
            println!("stopped");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_kill(mgr: &VpnManager) -> i32 {
    match mgr.force_kill() {
        Ok(()) => {
            println!("killed");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_restart(mgr: &VpnManager, cfg: &PinglingConfig) -> i32 {
    match mgr.restart() {
        Ok(()) => {
            println!("restarted ({})", cfg.core_type);
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_status(mgr: &VpnManager) -> i32 {
    let status = mgr.get_status();
    println!("{status}");
    0
}

fn cmd_info(mgr: &VpnManager) -> i32 {
    let info = mgr.get_core_info();
    println!("name:      {}", info.name);
    println!("version:   {}", info.version);
    println!("protocols: {}", info.supported_protocols.join(", "));
    0
}

fn cmd_validate(mgr: &VpnManager, config_path: &Option<String>, cfg: &PinglingConfig) -> i32 {
    let path = match config_path {
        Some(p) => p.clone(),
        None => cfg.core_config_path.clone(),
    };

    if path.is_empty() {
        eprintln!("error: no config path provided. Use the argument or set core_config_path");
        return 1;
    }

    match mgr.validate_config(&path) {
        Ok(()) => {
            println!("valid: {path}");
            0
        }
        Err(e) => {
            eprintln!("invalid: {e}");
            1
        }
    }
}

fn cmd_prereqs(mgr: &VpnManager) -> i32 {
    let checks = mgr.check_prerequisites();
    let mut all_passed = true;

    for check in &checks {
        let mark = if check.passed { "ok" } else { "FAIL" };
        println!("[{mark}] {}: {}", check.name, check.message);
        if !check.passed {
            all_passed = false;
        }
    }

    if all_passed {
        println!("\nall prerequisites met");
        0
    } else {
        eprintln!("\nsome prerequisites not met");
        1
    }
}
