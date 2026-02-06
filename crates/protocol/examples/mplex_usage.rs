//! Example demonstrating the multiplex I/O API.
//!
//! This example shows how to use MplexWriter and MplexReader to transparently
//! multiplex data and control messages over a single stream.

use std::io::{self, Cursor, Read, Write};
use protocol::{MplexReader, MplexWriter};

fn main() -> io::Result<()> {
    println!("=== Rsync Multiplex I/O Example ===\n");

    // Create a buffer to act as our "network connection"
    let mut stream = Vec::new();

    // === Writing Side ===
    println!("Writing multiplexed data...");
    {
        let mut writer = MplexWriter::new(&mut stream);

        // Send informational message
        writer.write_info("Transfer starting\n")?;

        // Write some file data (automatically framed as MSG_DATA)
        writer.write_all(b"Hello, ")?;
        writer.write_all(b"rsync ")?;
        writer.write_all(b"world!")?;

        // Send a warning
        writer.write_warning("Network latency detected\n")?;

        // Write more data
        writer.write_all(b"\nSecond chunk of data.")?;

        // Flush to ensure everything is sent
        writer.flush()?;
    }

    println!("Wrote {} bytes to stream\n", stream.len());

    // === Reading Side ===
    println!("Reading multiplexed data...");
    {
        let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let messages_clone = messages.clone();

        let mut reader = MplexReader::new(Cursor::new(&stream));

        // Set up message handler for control messages
        reader.set_message_handler(move |code, payload| {
            if let Ok(msg) = std::str::from_utf8(payload) {
                messages_clone
                    .lock()
                    .unwrap()
                    .push((code, msg.to_string()));
            }
        });

        // Read data normally - control messages are handled automatically
        let mut data = String::new();
        let mut buffer = [0u8; 64];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break, // Empty frame or end
                Ok(n) => {
                    data.push_str(&String::from_utf8_lossy(&buffer[..n]));
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        println!("Data received: {:?}", data);
        println!("\nControl messages:");
        for (code, msg) in messages.lock().unwrap().iter() {
            println!("  [{:?}] {}", code, msg.trim());
        }
    }

    // === Advanced Usage: Large Data ===
    println!("\n=== Large Data Transfer ===");
    {
        let mut stream = Vec::new();
        let large_data = vec![0x42u8; 50_000];

        // Write large data (will be split into multiple frames automatically)
        {
            let mut writer = MplexWriter::new(&mut stream);
            writer.write_all(&large_data)?;
            writer.flush()?;
        }

        println!("Wrote {} bytes of data", large_data.len());
        println!("Stream size: {} bytes (includes frame overhead)", stream.len());

        // Read it back
        {
            let mut reader = MplexReader::new(Cursor::new(&stream));
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => received.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
            }

            println!("Read {} bytes back", received.len());
            assert_eq!(received, large_data);
            println!("Data integrity verified!");
        }
    }

    println!("\n=== Example Complete ===");
    Ok(())
}
