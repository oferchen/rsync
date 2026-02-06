//! Demonstration of hardlink detection and resolution.
//!
//! This example shows how to use the `HardlinkTracker` to detect and resolve
//! hardlinks in a file list, matching the upstream rsync algorithm.

use engine::hardlink::{HardlinkAction, HardlinkKey, HardlinkTracker};

fn main() {
    println!("=== Hardlink Detection Demo ===\n");

    // Create a tracker
    let mut tracker = HardlinkTracker::new();

    println!("Simulating file list scan with hardlinks:\n");

    // Simulate scanning a directory with hardlinked files
    // Device 0xFD00, multiple inodes

    // Group 1: /usr/bin/vim -> /usr/bin/vi (common hardlink)
    let vim_ino = HardlinkKey::new(0xFD00, 100);
    println!("File 0: /usr/bin/vim  (dev={:#x}, ino={})", vim_ino.dev, vim_ino.ino);
    tracker.register(vim_ino, 0);

    println!("File 1: /usr/bin/vi   (dev={:#x}, ino={})", vim_ino.dev, vim_ino.ino);
    tracker.register(vim_ino, 1);

    // Single file (no hardlinks)
    let readme_ino = HardlinkKey::new(0xFD00, 200);
    println!("File 2: /home/README  (dev={:#x}, ino={})", readme_ino.dev, readme_ino.ino);
    tracker.register(readme_ino, 2);

    // Group 2: Multiple hardlinks to one inode
    let lib_ino = HardlinkKey::new(0xFD00, 300);
    println!("File 3: /lib/libc.so.6     (dev={:#x}, ino={})", lib_ino.dev, lib_ino.ino);
    tracker.register(lib_ino, 3);

    println!("File 4: /lib/libc-2.31.so  (dev={:#x}, ino={})", lib_ino.dev, lib_ino.ino);
    tracker.register(lib_ino, 4);

    println!("File 5: /lib/libc.so       (dev={:#x}, ino={})", lib_ino.dev, lib_ino.ino);
    tracker.register(lib_ino, 5);

    // Cross-device: same inode, different device (should NOT be linked)
    let other_dev_ino = HardlinkKey::new(0xFD01, 100);
    println!("File 6: /mnt/data/file (dev={:#x}, ino={})", other_dev_ino.dev, other_dev_ino.ino);
    tracker.register(other_dev_ino, 6);

    println!("\n=== Transfer Actions ===\n");

    // Now resolve what action to take for each file
    for i in 0..7 {
        let action = tracker.resolve(i);
        let file_name = match i {
            0 => "/usr/bin/vim",
            1 => "/usr/bin/vi",
            2 => "/home/README",
            3 => "/lib/libc.so.6",
            4 => "/lib/libc-2.31.so",
            5 => "/lib/libc.so",
            6 => "/mnt/data/file",
            _ => unreachable!(),
        };

        match action {
            HardlinkAction::Transfer => {
                println!("File {}: {} - TRANSFER (full file)", i, file_name);
            }
            HardlinkAction::LinkTo(source) => {
                let source_name = match source {
                    0 => "/usr/bin/vim",
                    3 => "/lib/libc.so.6",
                    _ => unreachable!(),
                };
                println!("File {}: {} - LINK to file {} ({})", i, file_name, source, source_name);
            }
            HardlinkAction::Skip => {
                println!("File {}: {} - SKIP", i, file_name);
            }
        }
    }

    println!("\n=== Statistics ===\n");
    println!("Total files registered: {}", tracker.file_count());
    println!("Hardlink groups (with 2+ files): {}", tracker.group_count());

    println!("\n=== Hardlink Groups ===\n");
    for (i, group) in tracker.groups().enumerate() {
        println!("Group {}: (dev={:#x}, ino={})", i + 1, group.key.dev, group.key.ino);
        println!("  Source: file {}", group.source_index);
        println!("  Links:  {:?}", group.link_indices);
        println!("  Total files in group: {}", group.total_count());
    }

    println!("\n=== Protocol Encoding Simulation ===\n");

    // Simulate protocol 30+ encoding
    println!("Protocol 30+ (uses indices):");
    for group in tracker.groups() {
        println!("  Source file {} gets XMIT_HLINKED | XMIT_HLINK_FIRST", group.source_index);
        for &link_idx in &group.link_indices {
            println!("  Link file {} gets XMIT_HLINKED, write index {}", link_idx, group.source_index);
        }
    }

    println!("\nProtocol 28-29 (uses dev/ino):");
    for group in tracker.groups() {
        println!("  Files with (dev={:#x}, ino={}):", group.key.dev, group.key.ino);
        println!("    First file {}: write dev={:#x}, ino={}",
                 group.source_index, group.key.dev, group.key.ino);
        for &link_idx in &group.link_indices {
            println!("    Link file {}: write same dev flag, ino={}",
                     link_idx, group.key.ino);
        }
    }
}
