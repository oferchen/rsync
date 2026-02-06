//! Example demonstrating rsyncd.conf parsing.
//!
//! Run with:
//! ```
//! cargo run --package daemon --example parse_config /etc/rsyncd.conf
//! ```

use daemon::rsyncd_config::RsyncdConfig;
use std::env;
use std::path::Path;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <path-to-rsyncd.conf>", args[0]);
        process::exit(1);
    }

    let config_path = Path::new(&args[1]);
    let config = match RsyncdConfig::from_file(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error parsing config: {}", e);
            process::exit(1);
        }
    };

    println!("Configuration loaded successfully!");
    println!();

    // Display global settings
    println!("Global Settings:");
    println!("  Port: {}", config.global().port());
    if let Some(addr) = config.global().address() {
        println!("  Address: {}", addr);
    }
    if let Some(motd) = config.global().motd_file() {
        println!("  MOTD file: {}", motd.display());
    }
    if let Some(log) = config.global().log_file() {
        println!("  Log file: {}", log.display());
    }
    if let Some(pid) = config.global().pid_file() {
        println!("  PID file: {}", pid.display());
    }
    println!();

    // Display modules
    println!("Modules ({} total):", config.modules().len());
    for module in config.modules() {
        println!();
        println!("  [{}]", module.name());
        println!("    Path: {}", module.path().display());

        if let Some(comment) = module.comment() {
            println!("    Comment: {}", comment);
        }

        println!("    Read-only: {}", module.read_only());
        println!("    Write-only: {}", module.write_only());
        println!("    List: {}", module.list());
        println!("    Use chroot: {}", module.use_chroot());
        println!("    Numeric IDs: {}", module.numeric_ids());

        if let Some(uid) = module.uid() {
            println!("    UID: {}", uid);
        }
        if let Some(gid) = module.gid() {
            println!("    GID: {}", gid);
        }

        if module.max_connections() > 0 {
            println!("    Max connections: {}", module.max_connections());
        }

        if !module.auth_users().is_empty() {
            println!("    Auth users: {}", module.auth_users().join(", "));
            if let Some(secrets) = module.secrets_file() {
                println!("    Secrets file: {}", secrets.display());
            }
        }

        if !module.hosts_allow().is_empty() {
            println!("    Hosts allow: {}", module.hosts_allow().join(", "));
        }
        if !module.hosts_deny().is_empty() {
            println!("    Hosts deny: {}", module.hosts_deny().join(", "));
        }

        if !module.refuse_options().is_empty() {
            println!("    Refuse options: {}", module.refuse_options().join(", "));
        }

        if let Some(timeout) = module.timeout() {
            println!("    Timeout: {} seconds", timeout);
        }
    }
}
