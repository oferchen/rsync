//! QUIC certificate validation matrix: the source of truth for which
//! certificate shapes the QUIC transport accepts and refuses.
//!
//! Every certificate is minted with `rcgen` when the test runs. Each case
//! starts the real `oc-rsync` daemon on loopback and drives the real client
//! against it, then pins the exit code and a stable fragment of the client's
//! (or, for identity-load cases, the daemon's) stderr. Cases that reach a
//! transfer run both a pull and a push, because the two directions build
//! their connections through different client code paths.
//!
//! Client QUIC trust is strict: `--quic-ca` is the only trust anchor and the
//! server chain is checked by the rustls WebPKI verifier (chain, validity,
//! `serverAuth` EKU, CA:FALSE leaf, SAN against the dialled host). A refused
//! handshake exits 5, the code a refused daemon connection has always used.
//! `docs/quic-transport.md` summarises these rows and names this file as the
//! authority.

#![cfg(all(unix, feature = "quic"))]

use std::ffi::OsString;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use test_support::Deadlined;
use test_support::quic_cert::{pem_block, rcgen};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose as Eku,
    IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType, SignatureAlgorithm,
};

/// Exit code of a refused QUIC handshake or trust setup (`RERR_STARTCLIENT`).
const REFUSED: i32 = 5;
/// Exit code of a daemon that cannot load its configuration.
const CONFIG_FATAL: i32 = 1;
/// rustls names the dialled host it could not match.
const NAME_MISMATCH: &str = "certificate not valid for name \"localhost\"";
/// Upper bound on any single client or daemon run. Well above a loopback
/// handshake, well below the QUIC idle timeout a hung case would hit.
const RUN_BUDGET: Duration = Duration::from_secs(20);

// -- certificate minting ----------------------------------------------------

/// A certificate authority that can sign leaves and intermediates.
struct Ca {
    cert_pem: String,
    issuer: Issuer<'static, KeyPair>,
}

/// A PEM certificate chain (leaf first) and the leaf's PKCS#8 key.
struct Identity {
    chain_pem: String,
    key_pem: String,
}

/// The shape of a leaf certificate under test.
struct LeafSpec {
    sans: Vec<SanType>,
    ekus: Vec<Eku>,
    ca: bool,
    validity: Option<Window>,
    key: KeyPair,
}
/// A validity window placed relative to the current clock, so no case
/// depends on a fixed calendar date.
#[derive(Clone, Copy)]
enum Window {
    /// Ended a day ago.
    Expired,
    /// Starts a day from now.
    NotYetValid,
}

impl LeafSpec {
    /// A conforming `localhost` server leaf: P-256, CA:FALSE, `serverAuth`.
    fn server() -> Self {
        Self {
            sans: vec![dns("localhost")],
            ekus: vec![Eku::ServerAuth],
            ca: false,
            validity: None,
            key: key_for(&rcgen::PKCS_ECDSA_P256_SHA256),
        }
    }

    /// A conforming client leaf for mutual TLS: P-256, CA:FALSE, `clientAuth`.
    fn client() -> Self {
        Self {
            sans: vec![dns("client.test")],
            ekus: vec![Eku::ClientAuth],
            ..Self::server()
        }
    }

    fn params(&self) -> CertificateParams {
        let mut params = CertificateParams::default();
        params.distinguished_name = common_name("oc-rsync matrix leaf");
        params.subject_alt_names = self.sans.clone();
        params.extended_key_usages = self.ekus.clone();
        if self.ca {
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        }
        if let Some(window) = self.validity {
            let since_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after the epoch");
            let now = rcgen::date_time_ymd(1970, 1, 1) + since_epoch;
            let day = Duration::from_secs(86_400);
            (params.not_before, params.not_after) = match window {
                Window::Expired => (now - 2 * day, now - day),
                Window::NotYetValid => (now + day, now + 2 * day),
            };
        }
        params
    }

    fn signed_by(self, ca: &Ca) -> Identity {
        let cert = self
            .params()
            .signed_by(&self.key, &ca.issuer)
            .expect("rcgen: sign leaf");
        Identity {
            chain_pem: pem_block("CERTIFICATE", cert.der()),
            key_pem: pem_block("PRIVATE KEY", &self.key.serialize_der()),
        }
    }

    fn self_signed(self) -> Identity {
        let cert = self
            .params()
            .self_signed(&self.key)
            .expect("rcgen: self-sign leaf");
        Identity {
            chain_pem: pem_block("CERTIFICATE", cert.der()),
            key_pem: pem_block("PRIVATE KEY", &self.key.serialize_der()),
        }
    }
}

fn dns(name: &str) -> SanType {
    SanType::DnsName(name.try_into().expect("rcgen: DNS SAN"))
}

fn key_for(alg: &'static SignatureAlgorithm) -> KeyPair {
    KeyPair::generate_for(alg).expect("rcgen: generate key")
}

/// A subject naming only `name`. Every CA gets its own, so an unrelated CA
/// shows up as an unknown issuer rather than a same-name signature mismatch.
fn common_name(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    dn
}

fn ca_params(name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

/// A fresh self-signed root CA. Two calls give two unrelated trust anchors.
fn root_ca(name: &str) -> Ca {
    let key = key_for(&rcgen::PKCS_ECDSA_P256_SHA256);
    let params = ca_params(name);
    let cert = params.self_signed(&key).expect("rcgen: self-sign CA");
    Ca {
        cert_pem: pem_block("CERTIFICATE", cert.der()),
        issuer: Issuer::new(params, key),
    }
}

/// An intermediate CA signed by `parent`.
fn intermediate_ca(parent: &Ca) -> Ca {
    let key = key_for(&rcgen::PKCS_ECDSA_P256_SHA256);
    let params = ca_params("oc-rsync test intermediate");
    let cert = params
        .signed_by(&key, &parent.issuer)
        .expect("rcgen: sign intermediate");
    Ca {
        cert_pem: pem_block("CERTIFICATE", cert.der()),
        issuer: Issuer::new(params, key),
    }
}

/// The PKCS#1 `RSAPublicKey` of an openssl-generated key, the form `rcgen`
/// embeds in a certificate's subject public key info.
struct RsaPublicKey(Vec<u8>);

impl rcgen::PublicKeyData for RsaPublicKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &rcgen::PKCS_RSA_SHA256
    }
}

/// A `localhost` server leaf with an RSA-2048 key, signed by `ca`.
///
/// The ring backend of `rcgen` cannot generate RSA keys, so `openssl genpkey`
/// makes the key and `rcgen` signs a certificate over its public half.
/// `None` when openssl is not installed.
fn rsa_2048_leaf(dir: &Path, ca: &Ca) -> Option<Identity> {
    let key = dir.join("rsa-key.pem");
    let public = dir.join("rsa-public.der");
    let openssl = |args: &[&str]| {
        Command::new("openssl")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let key_arg = key.to_str().expect("UTF-8 temp path");
    if !openssl(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:2048",
        "-out",
        key_arg,
    ]) {
        return None;
    }
    let public_arg = public.to_str().expect("UTF-8 temp path");
    assert!(
        openssl(&[
            "rsa",
            "-in",
            key_arg,
            "-RSAPublicKey_out",
            "-outform",
            "DER",
            "-out",
            public_arg
        ]),
        "openssl rsa: export the RSA public key"
    );
    let spki = RsaPublicKey(fs::read(public).expect("read RSA public key"));
    let cert = LeafSpec::server()
        .params()
        .signed_by(&spki, &ca.issuer)
        .expect("rcgen: sign RSA leaf");
    Some(Identity {
        chain_pem: pem_block("CERTIFICATE", cert.der()),
        key_pem: fs::read_to_string(key).expect("read RSA key"),
    })
}

// -- daemon and client harness ----------------------------------------------

/// The trust and identity files one case hands to the daemon and client.
#[derive(Default)]
struct Setup {
    server: Option<Identity>,
    /// Write the server key without the cert, or the cert without the key.
    server_cert_only: bool,
    server_key_only: bool,
    client_ca_for_daemon: Option<String>,
}

/// A scratch tree with a module, a pull source and a push source.
struct Scratch {
    dir: TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("module")).expect("module dir");
        fs::write(dir.path().join("module/served"), b"pulled over quic\n").expect("pull source");
        fs::write(dir.path().join("pushme"), b"pushed over quic\n").expect("push source");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, body).expect("write fixture file");
        path
    }

    fn config(&self, setup: &Setup) -> PathBuf {
        let mut globals = format!(
            "pid file = {}\nlock file = {}\nlog file = {}\nuse chroot = false\n",
            self.path("d.pid").display(),
            self.path("d.lock").display(),
            self.path("d.log").display(),
        );
        if let Some(server) = &setup.server {
            if !setup.server_key_only {
                let cert = self.write("server-chain.pem", &server.chain_pem);
                globals.push_str(&format!("quic cert file = {}\n", cert.display()));
            }
            if !setup.server_cert_only {
                let key = self.write("server-key.pem", &server.key_pem);
                globals.push_str(&format!("quic key file = {}\n", key.display()));
            }
        }
        if let Some(ca) = &setup.client_ca_for_daemon {
            let path = self.write("client-ca.pem", ca);
            globals.push_str(&format!("quic client ca file = {}\n", path.display()));
        }
        let body = format!(
            "{globals}\n[m]\npath = {}\nread only = false\n",
            self.path("module").display()
        );
        self.write("rsyncd.conf", &body)
    }
}

/// Kills the daemon on drop so a failing case never leaks a listener.
struct Daemon {
    child: Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn oc_bin() -> Option<PathBuf> {
    let bin = test_support::oc_rsync_bin();
    let help = Command::new(&bin).arg("--help").output().ok()?;
    if String::from_utf8_lossy(&help.stdout).contains("quic://") {
        Some(bin)
    } else {
        eprintln!(
            "skipping: {} was built without the `quic` feature",
            bin.display()
        );
        None
    }
}

/// Starts the daemon and waits for its QUIC listener.
fn spawn_quic_daemon(bin: &Path, scratch: &Scratch, setup: &Setup) -> Daemon {
    let config = scratch.config(setup);
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(bin)
            .args([
                "--daemon",
                "--no-detach",
                "--port",
                &port.to_string(),
                "--config",
            ])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    })
    .expect("spawn daemon");
    let daemon = Daemon { child, port };
    let log = scratch.path("d.log");
    let deadline = Instant::now() + RUN_BUDGET;
    while !fs::read_to_string(&log).is_ok_and(|text| text.contains("QUIC listener serving on")) {
        assert!(
            Instant::now() < deadline,
            "QUIC listener never came up: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    daemon
}

/// Runs the daemon expecting it to refuse its configuration and exit.
fn daemon_startup(bin: &Path, scratch: &Scratch, setup: &Setup) -> Outcome {
    let config = scratch.config(setup);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port();
    run(Command::new(bin)
        .args([
            "--daemon",
            "--no-detach",
            "--port",
            &port.to_string(),
            "--config",
        ])
        .arg(&config))
}

/// Exit code and stderr of one run. `None` means the run hit `RUN_BUDGET`.
struct Outcome {
    code: Option<i32>,
    stderr: String,
}

fn run(command: &mut Command) -> Outcome {
    match test_support::run_deadlined(command, RUN_BUDGET).expect("run oc-rsync") {
        Deadlined::Finished { status, stderr, .. } => Outcome {
            code: status.code(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        },
        Deadlined::Expired { stderr, .. } => Outcome {
            code: None,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Pull,
    Push,
}

/// Client-side trust and identity for one run.
#[derive(Default)]
struct ClientArgs {
    ca: Option<PathBuf>,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    host: Option<IpAddr>,
}

fn transfer(
    bin: &Path,
    scratch: &Scratch,
    daemon: &Daemon,
    direction: Direction,
    client: &ClientArgs,
) -> Outcome {
    let host = client
        .host
        .map_or_else(|| "localhost".to_owned(), |ip| ip.to_string());
    let remote = |path: &str| format!("quic://{host}:{}/m/{path}", daemon.port);
    let mut args: Vec<OsString> = vec!["-4".into()];
    for (flag, value) in [
        ("--quic-ca", &client.ca),
        ("--quic-cert", &client.cert),
        ("--quic-key", &client.key),
    ] {
        if let Some(path) = value {
            args.push(flag.into());
            args.push(path.into());
        }
    }
    match direction {
        Direction::Pull => {
            args.push(remote("served").into());
            args.push(scratch.path("pulled").into());
        }
        Direction::Push => {
            args.push(scratch.path("pushme").into());
            args.push(remote("pushed").into());
        }
    }
    run(Command::new(bin).args(&args))
}

/// Asserts `outcome` exited `code` and its stderr contains `fragment`.
#[track_caller]
fn expect(label: &str, outcome: &Outcome, code: i32, fragment: &str) {
    eprintln!(
        "[{label}] exit={:?} stderr={}",
        outcome.code,
        outcome.stderr.trim()
    );
    assert_eq!(
        outcome.code,
        Some(code),
        "{label}: exit code (stderr: {})",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains(fragment),
        "{label}: stderr must contain {fragment:?}: {}",
        outcome.stderr
    );
}

/// Runs a pull and a push against a daemon serving `setup` and pins both.
fn both_directions(
    setup: Setup,
    client: impl Fn(&Scratch) -> ClientArgs,
    code: i32,
    fragment: &str,
) {
    let Some(bin) = oc_bin() else { return };
    let scratch = Scratch::new();
    let daemon = spawn_quic_daemon(&bin, &scratch, &setup);
    let client = client(&scratch);
    for direction in [Direction::Pull, Direction::Push] {
        let outcome = transfer(&bin, &scratch, &daemon, direction, &client);
        expect(&format!("{direction:?}"), &outcome, code, fragment);
    }
    if code == 0 {
        assert_eq!(
            fs::read(scratch.path("pulled")).expect("pulled file"),
            b"pulled over quic\n"
        );
        assert_eq!(
            fs::read(scratch.path("module/pushed")).expect("pushed file"),
            b"pushed over quic\n"
        );
    } else {
        assert!(!scratch.path("pulled").exists(), "refused pull wrote data");
        assert!(
            !scratch.path("module/pushed").exists(),
            "refused push wrote data"
        );
    }
}

/// A daemon serving `server`, dialled by a client trusting `ca_pem`.
fn server_case(server: Identity, ca_pem: String, code: i32, fragment: &str) {
    let setup = Setup {
        server: Some(server),
        ..Setup::default()
    };
    both_directions(
        setup,
        |scratch| ClientArgs {
            ca: Some(scratch.write("trust.pem", &ca_pem)),
            ..ClientArgs::default()
        },
        code,
        fragment,
    );
}

// -- server certificate rows ------------------------------------------------

#[test]
fn server_ca_signed_leaf_with_dns_san_is_accepted() {
    let ca = root_ca("matrix ca");
    server_case(
        LeafSpec::server().signed_by(&ca),
        ca.cert_pem.clone(),
        0,
        "",
    );
}

#[test]
fn server_ip_san_dialled_by_ip_is_accepted() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        sans: vec![SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))],
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    let setup = Setup {
        server: Some(leaf),
        ..Setup::default()
    };
    let ca_pem = ca.cert_pem.clone();
    both_directions(
        setup,
        move |scratch| ClientArgs {
            ca: Some(scratch.write("trust.pem", &ca_pem)),
            host: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            ..ClientArgs::default()
        },
        0,
        "",
    );
}

#[test]
fn server_leaf_with_intermediate_chain_is_accepted() {
    let root = root_ca("matrix root");
    let intermediate = intermediate_ca(&root);
    let mut leaf = LeafSpec::server().signed_by(&intermediate);
    leaf.chain_pem.push_str(&intermediate.cert_pem);
    server_case(leaf, root.cert_pem.clone(), 0, "");
}

#[test]
fn server_leaf_marked_ca_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        ca: true,
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), REFUSED, "CaUsedAsEndEntity");
}

#[test]
fn server_san_mismatch_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        sans: vec![dns("not-localhost.test")],
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), REFUSED, NAME_MISMATCH);
}

#[test]
fn server_ip_san_dialled_by_name_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        sans: vec![SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))],
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), REFUSED, NAME_MISMATCH);
}

#[test]
fn server_leaf_without_server_auth_eku_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        ekus: vec![Eku::ClientAuth],
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(
        leaf,
        ca.cert_pem.clone(),
        REFUSED,
        "does not allow extended key usage for server authentication",
    );
}

#[test]
fn server_leaf_without_eku_extension() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        ekus: Vec::new(),
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), 0, "");
}

#[test]
fn server_expired_leaf_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        validity: Some(Window::Expired),
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), REFUSED, "certificate expired");
}

#[test]
fn server_not_yet_valid_leaf_is_refused() {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        validity: Some(Window::NotYetValid),
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(
        leaf,
        ca.cert_pem.clone(),
        REFUSED,
        "certificate not valid yet",
    );
}

#[test]
fn server_self_signed_leaf_with_unrelated_ca_is_refused() {
    let unrelated = root_ca("unrelated ca");
    server_case(
        LeafSpec::server().self_signed(),
        unrelated.cert_pem.clone(),
        REFUSED,
        "UnknownIssuer",
    );
}

#[test]
fn server_self_signed_leaf_named_as_its_own_trust_anchor() {
    let leaf = LeafSpec::server().self_signed();
    let anchor = leaf.chain_pem.clone();
    server_case(leaf, anchor, 0, "");
}

#[test]
fn server_leaf_from_a_different_ca_is_refused() {
    let signer = root_ca("signing ca");
    let other = root_ca("other ca");
    server_case(
        LeafSpec::server().signed_by(&signer),
        other.cert_pem.clone(),
        REFUSED,
        "UnknownIssuer",
    );
}

fn key_type_case(key: KeyPair) {
    let ca = root_ca("matrix ca");
    let leaf = LeafSpec {
        key,
        ..LeafSpec::server()
    }
    .signed_by(&ca);
    server_case(leaf, ca.cert_pem.clone(), 0, "");
}

#[test]
fn server_key_p256() {
    key_type_case(key_for(&rcgen::PKCS_ECDSA_P256_SHA256));
}

#[test]
fn server_key_p384() {
    key_type_case(key_for(&rcgen::PKCS_ECDSA_P384_SHA384));
}

#[test]
fn server_key_ed25519() {
    key_type_case(key_for(&rcgen::PKCS_ED25519));
}

#[test]
fn server_key_rsa2048() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let ca = root_ca("matrix ca");
    let Some(leaf) = rsa_2048_leaf(scratch.path(), &ca) else {
        eprintln!("skipping: openssl is not installed, so no RSA key can be generated");
        return;
    };
    server_case(leaf, ca.cert_pem.clone(), 0, "");
}

// -- daemon identity rows ---------------------------------------------------

fn identity_case(setup: Setup, fragment: &str) {
    let Some(bin) = oc_bin() else { return };
    let scratch = Scratch::new();
    let outcome = daemon_startup(&bin, &scratch, &setup);
    expect("daemon", &outcome, CONFIG_FATAL, fragment);
}

#[test]
fn daemon_cert_and_key_mismatch_exits_1() {
    let ca = root_ca("matrix ca");
    let mut server = LeafSpec::server().signed_by(&ca);
    server.key_pem = LeafSpec::server().signed_by(&ca).key_pem;
    identity_case(
        Setup {
            server: Some(server),
            ..Setup::default()
        },
        "the private key does not match the certificate",
    );
}

#[test]
fn daemon_cert_without_key_exits_1() {
    let ca = root_ca("matrix ca");
    identity_case(
        Setup {
            server: Some(LeafSpec::server().signed_by(&ca)),
            server_cert_only: true,
            ..Setup::default()
        },
        "quic key file",
    );
}

#[test]
fn daemon_key_without_cert_exits_1() {
    let ca = root_ca("matrix ca");
    identity_case(
        Setup {
            server: Some(LeafSpec::server().signed_by(&ca)),
            server_key_only: true,
            ..Setup::default()
        },
        "quic cert file",
    );
}

// -- client trust rows ------------------------------------------------------

fn client_trust_case(ca: impl Fn(&Scratch) -> Option<PathBuf>, fragment: &str) {
    let server_ca = root_ca("matrix ca");
    let setup = Setup {
        server: Some(LeafSpec::server().signed_by(&server_ca)),
        ..Setup::default()
    };
    both_directions(
        setup,
        |scratch| ClientArgs {
            ca: ca(scratch),
            ..ClientArgs::default()
        },
        REFUSED,
        fragment,
    );
}

#[test]
fn client_without_quic_ca_is_refused() {
    client_trust_case(|_| None, "--quic-ca");
}

#[test]
fn client_quic_ca_that_is_not_a_certificate_is_refused() {
    client_trust_case(
        |scratch| Some(scratch.write("not-a-cert.pem", &LeafSpec::server().self_signed().key_pem)),
        "no certificates found in --quic-ca file",
    );
}

#[test]
fn client_empty_quic_ca_is_refused() {
    client_trust_case(
        |scratch| Some(scratch.write("empty.pem", "")),
        "no certificates found in --quic-ca file",
    );
}

#[test]
fn client_unreadable_quic_ca_is_refused() {
    if rustix::process::geteuid().is_root() {
        eprintln!("skipping: root reads a mode-000 file, so the case cannot be built");
        return;
    }
    client_trust_case(
        |scratch| {
            let path = scratch.write("unreadable.pem", &root_ca("matrix ca").cert_pem);
            let mut perms = fs::metadata(&path).expect("stat").permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
            fs::set_permissions(&path, perms).expect("chmod 000");
            Some(path)
        },
        "reading --quic-ca file",
    );
}

// -- mutual TLS rows --------------------------------------------------------

/// A daemon requiring client certificates signed by `client_ca`, dialled by a
/// client presenting `client` (if any).
fn mtls_case(client_ca: &Ca, client: Option<Identity>, code: i32, fragment: &str) {
    let server_ca = root_ca("server ca");
    let setup = Setup {
        server: Some(LeafSpec::server().signed_by(&server_ca)),
        client_ca_for_daemon: Some(client_ca.cert_pem.clone()),
        ..Setup::default()
    };
    let server_ca_pem = server_ca.cert_pem.clone();
    both_directions(
        setup,
        move |scratch| ClientArgs {
            ca: Some(scratch.write("trust.pem", &server_ca_pem)),
            cert: client
                .as_ref()
                .map(|id| scratch.write("client-chain.pem", &id.chain_pem)),
            key: client
                .as_ref()
                .map(|id| scratch.write("client-key.pem", &id.key_pem)),
            ..ClientArgs::default()
        },
        code,
        fragment,
    );
}

#[test]
fn mtls_valid_client_certificate_is_accepted() {
    let client_ca = root_ca("client ca");
    let client = LeafSpec::client().signed_by(&client_ca);
    mtls_case(&client_ca, Some(client), 0, "");
}

#[test]
fn mtls_missing_client_certificate_is_refused() {
    mtls_case(
        &root_ca("client ca"),
        None,
        REFUSED,
        "peer sent no certificates",
    );
}

#[test]
fn mtls_client_certificate_from_wrong_ca_is_refused() {
    let client_ca = root_ca("client ca");
    let client = LeafSpec::client().signed_by(&root_ca("wrong ca"));
    mtls_case(&client_ca, Some(client), REFUSED, "UnknownIssuer");
}

#[test]
fn mtls_expired_client_certificate_is_refused() {
    let client_ca = root_ca("client ca");
    let client = LeafSpec {
        validity: Some(Window::Expired),
        ..LeafSpec::client()
    }
    .signed_by(&client_ca);
    mtls_case(&client_ca, Some(client), REFUSED, "certificate expired");
}

#[test]
fn mtls_client_certificate_without_client_auth_eku_is_refused() {
    let client_ca = root_ca("client ca");
    let client = LeafSpec {
        ekus: vec![Eku::ServerAuth],
        ..LeafSpec::client()
    }
    .signed_by(&client_ca);
    mtls_case(
        &client_ca,
        Some(client),
        REFUSED,
        "does not allow extended key usage for client authentication",
    );
}

#[test]
fn client_quic_cert_without_quic_key_is_refused() {
    let server_ca = root_ca("server ca");
    let setup = Setup {
        server: Some(LeafSpec::server().signed_by(&server_ca)),
        ..Setup::default()
    };
    let server_ca_pem = server_ca.cert_pem.clone();
    let client = LeafSpec::client().signed_by(&root_ca("client ca"));
    both_directions(
        setup,
        move |scratch| ClientArgs {
            ca: Some(scratch.write("trust.pem", &server_ca_pem)),
            cert: Some(scratch.write("client-chain.pem", &client.chain_pem)),
            ..ClientArgs::default()
        },
        REFUSED,
        "--quic-cert requires --quic-key",
    );
}

#[test]
fn client_certificate_offered_to_a_daemon_without_client_ca() {
    let server_ca = root_ca("server ca");
    let setup = Setup {
        server: Some(LeafSpec::server().signed_by(&server_ca)),
        ..Setup::default()
    };
    let server_ca_pem = server_ca.cert_pem.clone();
    let client = LeafSpec::client().signed_by(&root_ca("client ca"));
    both_directions(
        setup,
        move |scratch| ClientArgs {
            ca: Some(scratch.write("trust.pem", &server_ca_pem)),
            cert: Some(scratch.write("client-chain.pem", &client.chain_pem)),
            key: Some(scratch.write("client-key.pem", &client.key_pem)),
            ..ClientArgs::default()
        },
        0,
        "",
    );
}
