# Upstream rsync stunnel/SSL Integration Model (TLS-1)

Audit of how upstream rsync (3.1.0 through 3.4.x) handles SSL/TLS-encrypted
daemon connections. This covers the `rsync-ssl` helper script, stunnel
integration, proxy-based alternatives, and the implications for oc-rsync.

---

## Executive Summary

Upstream rsync has **no native TLS implementation**. All encryption for daemon
connections is delegated to external tools:

1. **`rsync-ssl` helper script** - wraps rsync with `--rsh` to tunnel through
   OpenSSL `s_client`, GnuTLS `gnutls-cli`, or stunnel.
2. **Reverse proxy** - haproxy or nginx terminates TLS on port 874 and
   forwards plaintext to the rsync daemon on localhost:873.
3. **stunnel server-side** - stunnel accepts TLS on port 874, spawns rsync in
   `--server --daemon` mode via `exec`.

The rsync daemon binary itself never links against any TLS library for
transport. OpenSSL is used only for MD4/MD5 checksum acceleration
(`EVP_MD_CTX_copy`), not for connection encryption.

---

## 1. The `rsync-ssl` Helper Script

### Purpose

`rsync-ssl` is a bash wrapper shipped with rsync since 3.1.0. It intercepts
daemon-style rsync invocations (`rsync://host/module` or `host::module`) and
injects an `--rsh` option that establishes a TLS tunnel before the rsync
protocol handshake begins.

### Invocation Flow

```
User invokes:
  rsync-ssl -aiv example.com::module/ /dest

Script rewrites to:
  rsync --rsh="rsync-ssl --HELPER" -aiv example.com::module/ /dest

rsync calls the --rsh helper:
  rsync-ssl --HELPER example.com rsync --server --daemon .

Helper establishes TLS tunnel, then rsync protocol runs over it.
```

The `--HELPER` flag is an internal mechanism - not user-facing. rsync treats
the `--rsh` program as a transport and speaks the daemon protocol over its
stdin/stdout.

### TLS Backend Selection

The script supports three backends, auto-detected in order of preference:

| Priority | Backend | Tool | Notes |
|----------|---------|------|-------|
| 1 | OpenSSL | `openssl s_client` | Preferred. Hostname verification, system CA support. |
| 2 | GnuTLS | `gnutls-cli` | Preliminary support. Known output-dropping bugs at release. |
| 3 | stunnel | `stunnel4` / `stunnel` | Requires v4+. No default CA verification. |

Override via `--type=openssl|stunnel` argument or `RSYNC_SSL_TYPE` environment
variable.

### Port Convention

- **Default TLS port: 874** (one above the standard rsync daemon port 873).
- Overridden via `RSYNC_SSL_PORT` or `RSYNC_PORT` environment variables.
- `RSYNC_PORT` takes precedence if set to a non-zero value.

### Certificate and Verification Handling

Controlled entirely through environment variables:

| Variable | Purpose |
|----------|---------|
| `RSYNC_SSL_CERT` | Client certificate file (mutual TLS) |
| `RSYNC_SSL_KEY` | Client private key file |
| `RSYNC_SSL_CA_CERT` | CA certificate for server verification |
| `RSYNC_SSL_OPENSSL` | Path to `openssl` binary |
| `RSYNC_SSL_GNUTLS` | Path to `gnutls-cli` binary |
| `RSYNC_SSL_STUNNEL` | Path to `stunnel` binary |

Three CA verification modes based on `RSYNC_SSL_CA_CERT`:

| State | OpenSSL behavior | stunnel behavior |
|-------|-----------------|-----------------|
| Unset (default) | System CAs, verification enforced | No verification (cannot use system CAs) |
| Empty string | No verification | `verifyChain = no` |
| Path to CA file | Specified CA, verification enforced | `CAfile` set, `verifyChain = yes` |

This is a critical asymmetry: **stunnel with default settings provides
encryption without any certificate validation**, making it vulnerable to
man-in-the-middle attacks. The OpenSSL backend is strictly safer by default.

### Backend Invocation Details

**OpenSSL mode:**
```bash
openssl s_client -verify_return_error -verify 4 \
  -quiet -verify_quiet \
  -servername $hostname -verify_hostname $hostname \
  -connect $hostname:$port
```
Supports SNI (`-servername`) and hostname verification (`-verify_hostname`).

**GnuTLS mode:**
```bash
gnutls-cli --logfile=/dev/null $hostname:$port
```

**stunnel mode** (no temp file - config via heredoc on fd 10):
```bash
stunnel -fd 10 <<EOF
foreground = yes
debug = crit
connect = $hostname:$port
client = yes
TIMEOUTclose = 0
EOF
```

---

## 2. Server-Side stunnel Configuration

### stunnel-rsyncd.conf.in

Upstream ships a stunnel configuration template installed to
`/etc/stunnel/rsyncd.conf` via `make install-ssl-daemon`. Key settings:

```ini
foreground = no
pid = /var/run/stunnel-rsyncd.pid
socket = l:TCP_NODELAY=1
socket = r:TCP_NODELAY=1
setuid = root        # Required for rsync chroot
setgid = root

[rsync]
accept = 874
cert = /etc/rsync-ssl/certs/server.crt
key  = /etc/rsync-ssl/certs/server.key
client = no
verify = 0           # Allow any SSL client (default)
CAfile = /etc/ssl/certs/ca-certificates.crt

# Alternative for mutual TLS:
# verify = 3
# CAfile = /etc/rsync-ssl/certs/allowed-clients.cert.pem

exec = /usr/bin/rsync
execargs = rsync --server --daemon .
```

### Architecture

stunnel operates in `exec` mode - it accepts a TLS connection on port 874,
terminates TLS, then spawns a fresh rsync process in `--server --daemon` mode.
This is similar to inetd-style invocation:

```
Client (TLS) --> stunnel:874 --> rsync --server --daemon .
```

Each connection spawns a new rsync process. stunnel handles the TLS layer;
rsync sees a plaintext pipe on stdin/stdout. The rsync daemon authentication
(MD4-based challenge-response) runs **inside** the TLS tunnel, providing
defense-in-depth.

### Certificate Paths

Default paths used by stunnel and the install target:
- Server cert: `/etc/rsync-ssl/certs/server.crt`
- Server key: `/etc/rsync-ssl/certs/server.key`
- CA bundle: `/etc/ssl/certs/ca-certificates.crt`

The `install-ssl-daemon` Makefile target checks for existing certificates and
prints a notice if they are missing.

---

## 3. Reverse Proxy Alternative

Since rsync 3.2.0, the recommended production approach has shifted from
stunnel to a reverse TLS proxy. The rsyncd.conf man page documents
configurations for haproxy and nginx.

### Proxy Protocol Support

The `proxy protocol` rsyncd.conf parameter (global, boolean, default `false`)
enables the PROXY protocol (v1 or v2). This lets the rsync daemon see the
real client IP through the proxy, enabling IP-based access controls and
accurate logging.

**Security warning from upstream:** enabling `proxy protocol` without
restricting direct access to port 873 allows attackers to spoof source IPs
by connecting to the backend port directly with a forged PROXY header.

### HAProxy Example

```
frontend fe_rsync-ssl
   bind :::874 ssl crt /etc/letsencrypt/example.com/combined.pem
   mode tcp
   use_backend be_rsync

backend be_rsync
   mode tcp
   server local-rsync 127.0.0.1:873 check send-proxy
```

### nginx Example

```
stream {
   server {
       listen 874 ssl;
       listen [::]:874 ssl;
       ssl_certificate /etc/letsencrypt/example.com/fullchain.pem;
       ssl_certificate_key /etc/letsencrypt/example.com/privkey.pem;
       proxy_pass localhost:873;
       proxy_protocol on;
       proxy_timeout 1m;
       proxy_connect_timeout 5s;
   }
}
```

### stunnel vs. Proxy Comparison

| Aspect | stunnel (exec) | Reverse proxy (haproxy/nginx) |
|--------|---------------|-------------------------------|
| Process model | New rsync per connection | Persistent daemon + proxy |
| Client IP preservation | Visible (stunnel runs rsync directly) | Via PROXY protocol |
| Certificate management | Manual PEM files | Let's Encrypt integration |
| Connection overhead | TLS handshake + process spawn | TLS handshake + TCP forwarding |
| Operational complexity | Two configs (stunnel + rsyncd) | Two configs (proxy + rsyncd) |
| Maintainer recommendation | Legacy | **Preferred** (since 3.2.0) |

---

## 4. What rsync Does NOT Do

- **No TLS library linking for transport.** rsync never calls `SSL_read()`,
  `SSL_write()`, or any TLS API for connection encryption. OpenSSL is
  linked only for checksum acceleration (`--enable-openssl` controls
  `EVP_MD_CTX_copy` for MD4/MD5).

- **No `--ssl` or `--ssl-daemon` flags.** These flags do not exist in
  upstream rsync. The rsync binary has no SSL/TLS awareness.

- **No certificate handling.** rsync itself never reads, validates, or
  presents certificates. All PKI is handled by the external TLS terminator.

- **No TLS protocol negotiation.** The daemon protocol (`@RSYNCD:`) starts
  immediately on the connection. TLS must be fully established before the
  first rsync byte.

---

## 5. Historical Native TLS Attempts

### The openssl-support.diff Patch

An early patch (`openssl-support.diff`) attempted to add native OpenSSL
support directly into rsync. As of 2008, the patch was reported non-functional
and was never maintained. It was formally abandoned in favor of the wrapper
script approach.

### GPL3 / OpenSSL License Conflict

rsync is GPLv3. OpenSSL's license was historically incompatible with GPLv3 for
linking purposes (requiring an explicit exemption clause). This legal barrier
contributed to the decision to avoid native OpenSSL integration for transport.
GitHub issue #30 on the rsync repository tracked this - it was closed, but the
project continued to avoid direct TLS linking for transport.

OpenSSL 3.0+ adopted the Apache-2.0 license, which is GPLv3-compatible,
potentially removing this legal barrier. However, upstream has shown no
interest in revisiting native TLS.

### Maintainer Position

Wayne Davison (upstream maintainer) has explicitly recommended the proxy-based
approach over stunnel and has not advocated for native TLS in rsync. The
architecture intentionally separates encryption from the file synchronization
protocol.

---

## 6. Third-Party Approaches

### VolSync (Kubernetes)

The VolSync project (for Kubernetes persistent volume replication) uses an
"rsync-tls" method that wraps rsync with stunnel in containers - not a native
TLS implementation. This confirms the pattern: even modern deployments use
external TLS wrappers with unmodified rsync.

### Samba Project

Samba's rsync daemon infrastructure requires SSL for rsync daemon connections
(`rsync-ssl` is now considered mainstream rather than a separate package). This
further validates the external-wrapper model.

---

## 7. Implications for oc-rsync

### Options

| Approach | Effort | Wire compatibility | Dependencies |
|----------|--------|--------------------|-------------|
| A. Stunnel passthrough | None | Perfect | External stunnel |
| B. Proxy passthrough | None | Perfect | External haproxy/nginx |
| C. Native rustls in daemon | Medium | Compatible (TLS terminates before protocol) | `rustls`, `tokio-rustls` |
| D. Native rustls + `rsync-ssl` compat | Medium-High | Must respond to OpenSSL `s_client` connections | `rustls`, `tokio-rustls` |

### Key Observations

1. **oc-rsync already works with stunnel and reverse proxies.** Since TLS
   terminates before the rsync protocol starts, the daemon sees a plaintext
   connection regardless of the TLS wrapper. No code changes are needed for
   approaches A and B.

2. **Native rustls would be a departure from upstream.** Upstream deliberately
   avoids TLS in the daemon binary. Adding native TLS to oc-rsync creates a
   feature divergence, but one that adds security value without protocol
   incompatibility.

3. **PROXY protocol support is a prerequisite.** Whether using external
   proxies or native TLS, the `proxy protocol` rsyncd.conf parameter should
   be implemented to preserve client IP visibility. This is independent of
   the TLS decision.

4. **rustls avoids the GPL/OpenSSL licensing issue.** rustls is Apache-2.0 /
   ISC / MIT licensed - fully GPLv3-compatible. This removes the legal
   barrier that historically blocked native TLS in upstream rsync.

5. **The `rsync-ssl` script expects a standard TLS listener.** If oc-rsync
   adds native TLS to its daemon, it must accept standard TLS connections on
   port 874 (or configurable). The `rsync-ssl` script would work against it
   without modification - it simply needs a TLS endpoint that pipes plaintext
   rsync protocol after the handshake.

6. **Challenge-response auth runs inside TLS.** The MD4-based daemon
   authentication is a separate layer from TLS. Both can coexist: TLS
   provides transport encryption, while rsync auth provides module-level
   access control.

### Recommendation

Support all three approaches:

- **Short term:** Document that oc-rsync works with stunnel and reverse
  proxies today (zero effort).
- **Medium term:** Implement PROXY protocol v1/v2 parsing in the daemon
  crate (enables reverse proxy deployments with IP preservation).
- **Long term:** Consider native rustls as an opt-in daemon feature
  (`--ssl-port`, `--ssl-cert`, `--ssl-key`). This would be a competitive
  advantage over upstream while maintaining full wire compatibility.

---

## Appendix A: File Inventory in Upstream rsync

| File | Purpose |
|------|---------|
| `rsync-ssl` | Bash helper script (client-side TLS wrapper) |
| `rsync-ssl.1.md` | Man page for rsync-ssl |
| `stunnel-rsyncd.conf.in` | Server-side stunnel config template |
| `tls.c` | **Unrelated** - "Trivial LS" utility for directory comparison |
| `configure.ac` | `--disable-openssl` (checksums only), `--with-openssl-conf` |
| `Makefile.in` | `install-ssl-daemon` target, `rsync-ssl` install |
| `rsyncd.conf.5.md` | Documents `proxy protocol` param and proxy examples |

## Appendix B: Version History

| Version | SSL/TLS Change |
|---------|---------------|
| 3.1.0 (2013) | `rsync-ssl` script and `stunnel-rsyncd.conf.in` introduced |
| 3.2.0 (2020) | OpenSSL and GnuTLS backends added to `rsync-ssl`; `proxy protocol` rsyncd.conf parameter added; script installed by default |
| 3.2.1 (2020) | Enhanced `rsync-ssl` capabilities |
| 3.4.x (2025) | No native TLS changes; proxy approach remains recommended |
