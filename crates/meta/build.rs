fn main() {
    if std::env::var_os("CARGO_FEATURE_ACL").is_some() {
        println!("cargo:rustc-link-lib=acl");
    }
}
