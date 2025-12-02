//! Example demonstrating embedded server mode with stdio.
//!
//! This example shows how to run the rsync server programmatically without
//! constructing CLI arguments. The server reads from stdin and writes to stdout,
//! typically when invoked over SSH or other transport.
//!
//! Run with: cargo run --example server_stdio

use embedding::{ServerConfig, ServerRole, ServerStats, run_server_with_config};
use std::io;

fn main() {
    // Parse server configuration from command-line arguments or construct programmatically
    // This example uses typical server flag string for a receiver
    let config = match ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        "-logDtpre.iLsfxC".to_string(),
        vec![".".into()],
    ) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to create server config: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("[example] Server mode: {:?}", config.role);
    eprintln!("[example] Flag string: {}", config.flag_string);
    eprintln!("[example] Arguments: {:?}", config.args);
    eprintln!("[example] Waiting for client connection on stdin...");

    // Run server with stdio
    // In production, stdin/stdout would be connected to SSH or other transport
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    match run_server_with_config(config, &mut stdin, &mut stdout) {
        Ok(stats) => {
            eprintln!("[example] Server execution completed successfully");
            print_stats(&stats);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[example] Server execution failed: {e}");
            std::process::exit(1);
        }
    }
}

fn print_stats(stats: &ServerStats) {
    match stats {
        ServerStats::Receiver(transfer_stats) => {
            eprintln!("[example] === Receiver Statistics ===");
            eprintln!("[example] Files listed: {}", transfer_stats.files_listed);
            eprintln!(
                "[example] Files transferred: {}",
                transfer_stats.files_transferred
            );
            eprintln!(
                "[example] Total bytes received: {}",
                transfer_stats.bytes_received
            );
        }
        ServerStats::Generator(generator_stats) => {
            eprintln!("[example] === Generator Statistics ===");
            eprintln!("[example] Files listed: {}", generator_stats.files_listed);
            eprintln!(
                "[example] Files transferred: {}",
                generator_stats.files_transferred
            );
            eprintln!("[example] Total bytes sent: {}", generator_stats.bytes_sent);
        }
    }
}
