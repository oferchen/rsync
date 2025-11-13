use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if !cfg!(unix) {
        return;
    }

    if env::var_os("CARGO_FEATURE_ACL").is_none() {
        return;
    }

    if target_exposes_acl_via_libsystem() {
        return;
    }

    if target_triplet_mismatch() {
        println!("cargo:rustc-link-lib=dylib=acl");
        return;
    }

    if let Some(path) = locate_libacl() {
        if let Some(out_dir) = env::var_os("OUT_DIR") {
            let out_dir = PathBuf::from(out_dir);
            if let Some(link_dir) = ensure_link_proxy(&out_dir, &path) {
                println!("cargo:rustc-link-search={}", link_dir.display());
                println!("cargo:rustc-link-lib=dylib=acl");
                return;
            }
        }

        if let Some(dir) = path.parent() {
            println!("cargo:rustc-link-search={}", dir.display());
        }
        println!("cargo:rustc-link-lib=dylib=acl");
        return;
    }

    println!("cargo:rustc-link-lib=dylib=acl");
}

fn target_triplet_mismatch() -> bool {
    match (env::var("HOST"), env::var("TARGET")) {
        (Ok(host), Ok(target)) => host != target,
        _ => false,
    }
}

fn target_exposes_acl_via_libsystem() -> bool {
    // Apple's libSystem exposes the POSIX ACL symbols without an external libacl
    // shim, so adding an explicit `-lacl` linker flag would fail.
    matches!(env::var("CARGO_CFG_TARGET_VENDOR").as_deref(), Ok("apple"))
}

fn locate_libacl() -> Option<PathBuf> {
    let output = Command::new("ldconfig").arg("-p").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .filter_map(parse_ldconfig_line)
        .find(|path| path.exists())
}

fn parse_ldconfig_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if !trimmed.starts_with("libacl.so") {
        return None;
    }
    let (_, path) = trimmed.rsplit_once("=>")?;
    let candidate = PathBuf::from(path.trim());
    Some(candidate)
}

fn ensure_link_proxy(out_dir: &Path, source: &Path) -> Option<PathBuf> {
    let link_dir = out_dir.join("acl-link");
    fs::create_dir_all(&link_dir).ok()?;
    let target = link_dir.join("libacl.so");
    if target.exists() {
        return Some(link_dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if symlink(source, &target).is_ok() {
            return Some(link_dir);
        }
    }
    fs::copy(source, &target).ok()?;
    Some(link_dir)
}
