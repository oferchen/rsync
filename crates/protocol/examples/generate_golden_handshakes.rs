//! Generate reference golden handshake files from our protocol implementation.
//!
//! This example generates baseline golden files that can be used for regression
//! testing. These files represent our implementation's wire format and can be
//! validated against upstream rsync in integration tests.
//!
//! Usage:
//!   cargo run --example generate_golden_handshakes
//!
//! Output:
//!   Writes golden files to tests/protocol_handshakes/

use protocol::CompatibilityFlags;
use std::fs;
use std::path::Path;

fn main() -> std::io::Result<()> {
    println!("Generating reference golden handshake files...\n");

    // Find workspace root (go up from binary location)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let output_base = workspace_root.join("tests/protocol_handshakes");

    println!("Output directory: {}\n", output_base.display());

    // Generate legacy ASCII handshakes (protocols 28-29)
    generate_legacy_handshakes(&output_base)?;

    // Generate binary handshakes (protocols 30-32)
    generate_binary_handshakes(&output_base)?;

    println!("\n✓ Golden files generated successfully");
    println!("\nNote: These are reference files from our implementation.");
    println!("Validate against upstream rsync using: cargo test --workspace --test interop");

    Ok(())
}

fn generate_legacy_handshakes(base: &Path) -> std::io::Result<()> {
    println!("Legacy ASCII handshakes (protocols 28-29):");

    for protocol in [28, 29] {
        let dir = base.join(format!("protocol_{protocol}_legacy"));
        fs::create_dir_all(&dir)?;

        // Client greeting: @RSYNCD: <version>\n
        let greeting = format!("@RSYNCD: {protocol}.0\n");
        let greeting_path = dir.join("client_greeting.txt");
        fs::write(&greeting_path, greeting.as_bytes())?;
        println!("  ✓ {}", greeting_path.display());

        // Server response: @RSYNCD: OK\n
        // (Simplified - actual response may include additional info)
        let response = format!("@RSYNCD: {protocol}.0\n");
        let response_path = dir.join("server_response.txt");
        fs::write(&response_path, response.as_bytes())?;
        println!("  ✓ {}", response_path.display());
    }

    Ok(())
}

fn generate_binary_handshakes(base: &Path) -> std::io::Result<()> {
    println!("\nBinary handshakes (protocols 30-32):");

    for protocol_num in [30, 31, 32] {
        let dir = base.join(format!("protocol_{protocol_num}_binary"));
        fs::create_dir_all(&dir)?;

        // Generate client hello (simplified version list)
        let mut client_hello = Vec::new();
        // This is a simplified representation - actual implementation is more complex
        // Client would send supported protocol versions as varints
        protocol::write_varint(&mut client_hello, protocol_num)?;
        protocol::write_varint(&mut client_hello, 0)?; // End of list

        let hello_path = dir.join("client_hello.bin");
        fs::write(&hello_path, &client_hello)?;
        println!(
            "  ✓ {} ({} bytes)",
            hello_path.display(),
            client_hello.len()
        );

        // Generate server response (selected protocol)
        let mut server_response = Vec::new();
        protocol::write_varint(&mut server_response, protocol_num)?;

        let response_path = dir.join("server_response.bin");
        fs::write(&response_path, &server_response)?;
        println!(
            "  ✓ {} ({} bytes)",
            response_path.display(),
            server_response.len()
        );

        // For protocol 32, also generate compatibility flags exchange
        if protocol_num == 32 {
            let mut compat_exchange = Vec::new();

            // Example compat flags
            let flags = CompatibilityFlags::INC_RECURSE
                | CompatibilityFlags::SYMLINK_TIMES
                | CompatibilityFlags::SAFE_FILE_LIST
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS;

            flags.encode_to_vec(&mut compat_exchange)?;

            let compat_path = dir.join("compatibility_exchange.bin");
            fs::write(&compat_path, &compat_exchange)?;
            println!(
                "  ✓ {} ({} bytes)",
                compat_path.display(),
                compat_exchange.len()
            );
        }
    }

    Ok(())
}
