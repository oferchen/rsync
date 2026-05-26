# Encrypting daemon connections with TLS

Audience: operators and users who run `oc-rsync` in daemon mode (`oc-rsync --daemon`
or `oc-rsyncd`) and want encrypted transport between client and server.

## Overview

oc-rsync daemon mode listens on TCP port 873 and speaks the rsync wire
protocol in cleartext - the same as upstream rsync. Neither upstream rsync nor
oc-rsync ships a native TLS implementation. Instead, encryption is delegated
to an external tool that terminates TLS and forwards plain rsync bytes to the
daemon over loopback.

Three approaches are covered here, from simplest to most production-ready:

1. **stunnel** - the upstream-blessed pattern, widely documented in the rsync
   community.
2. **rsync-ssl / openssl s_client** - client-side TLS wrapping with no
   server-side changes beyond a certificate.
3. **Reverse proxy** (HAProxy or nginx) - recommended for production, with TLS
   offload, certificate rotation, and health checking.

All three work with an unmodified oc-rsync daemon. No code changes, feature
flags, or recompilation are needed.

### Port conventions

| Port | Protocol |
|------|----------|
| 873 | rsync cleartext (IANA-registered) |
| 874 | rsync over TLS (community convention, not IANA-registered) |

---

## Method 1: stunnel (server-side wrapping)

stunnel is the approach documented in the upstream rsync tarball
(`stunnel-rsyncd.conf`). It accepts TLS connections on port 874 and forwards
decrypted bytes to the daemon on loopback.

### Generate a self-signed certificate (testing only)

For production, use a certificate from your organization's CA or a public CA
via ACME (Let's Encrypt). For testing:

```sh
mkdir -p /etc/oc-rsync-ssl
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout /etc/oc-rsync-ssl/server.key \
    -out /etc/oc-rsync-ssl/server.crt \
    -days 365 \
    -subj "/CN=rsync.example.com" \
    -addext "subjectAltName=DNS:rsync.example.com"
chmod 600 /etc/oc-rsync-ssl/server.key
```

The `-addext "subjectAltName=..."` is required. Modern TLS libraries reject
certificates without a Subject Alternative Name.

### stunnel configuration

Create `/etc/stunnel/oc-rsyncd.conf`:

```ini
foreground = no
pid = /var/run/stunnel-oc-rsyncd.pid
socket = l:TCP_NODELAY=1
socket = r:TCP_NODELAY=1

[oc-rsync]
accept  = 874
connect = 127.0.0.1:8730
cert    = /etc/oc-rsync-ssl/server.crt
key     = /etc/oc-rsync-ssl/server.key
client  = no

# SECURITY: verify = 0 means no client certificate check (default).
# This prevents MitM only if clients verify the server certificate.
# For mutual TLS, set verify = 2 and supply a CAfile:
#   verify = 2
#   CAfile = /etc/oc-rsync-ssl/ca.crt
```

### stunnel exec mode (inetd-style)

stunnel can also spawn a fresh `oc-rsync --daemon` process per connection
instead of forwarding to a long-running daemon. This avoids running a
persistent oc-rsyncd process but costs a process fork per connection:

```ini
[oc-rsync]
accept  = 874
cert    = /etc/oc-rsync-ssl/server.crt
key     = /etc/oc-rsync-ssl/server.key
client  = no

exec    = /usr/local/bin/oc-rsync
execArgs = oc-rsync --daemon --no-detach --config /etc/oc-rsyncd.conf
```

In exec mode, stunnel itself handles TLS and passes the decrypted socket to
the spawned process via stdin/stdout.

### Daemon configuration

Configure the daemon to listen only on loopback so that the only path in is
through stunnel:

```ini
# /etc/oc-rsyncd.conf
address = 127.0.0.1
port    = 8730

[backups]
    path         = /srv/backups
    read only    = false
    auth users   = backup-bot
    secrets file = /etc/oc-rsyncd.secrets
    hosts allow  = 127.0.0.1
```

Setting `address = 127.0.0.1` prevents direct cleartext access from the
network. `hosts allow = 127.0.0.1` on each module is defense in depth.

### Start the services

```sh
# Start stunnel (reads /etc/stunnel/oc-rsyncd.conf by default)
stunnel

# Start the daemon
oc-rsync --daemon --config /etc/oc-rsyncd.conf
```

---

## Method 2: rsync-ssl / openssl s_client (client-side wrapping)

The client side needs its own TLS tunnel to connect to a TLS-wrapped daemon.
Two options:

### Option A: `--connect-program` with openssl

oc-rsync supports `--connect-program` to specify a custom transport command
for `rsync://` URLs. The placeholders `%H` (host) and `%P` (port) are
expanded at runtime:

```sh
oc-rsync \
    --connect-program 'openssl s_client -quiet -verify_quiet -servername %H -connect %H:%P' \
    -av rsync://daemon-host:874/backups/ ./restore/
```

To verify the server certificate against a specific CA:

```sh
oc-rsync \
    --connect-program 'openssl s_client -quiet -verify_quiet -CAfile /etc/oc-rsync-ssl/ca.crt -verify_return_error -servername %H -connect %H:%P' \
    -av rsync://daemon-host:874/backups/ ./restore/
```

Key flags:

- `-quiet` suppresses the TLS session info that would corrupt the rsync stream.
- `-verify_return_error` aborts the connection if certificate verification
  fails (without this flag, openssl warns but proceeds).
- `-CAfile` specifies the trusted CA bundle.

### Option B: upstream rsync-ssl wrapper script

The upstream rsync tarball includes an `rsync-ssl` script that automates the
openssl/stunnel client setup. It works with oc-rsync as a drop-in:

```sh
rsync-ssl -av daemon-host::backups/ ./restore/
```

Under the hood, `rsync-ssl` sets the `RSYNC_SSL_TYPE` environment variable and
launches the appropriate TLS client (`openssl`, `gnutls-cli`, or `stunnel`).
The variable `RSYNC_SSL_PORT` defaults to 874.

### Option C: local stunnel client

Run stunnel on the client host in client mode:

```ini
# /etc/stunnel/oc-rsync-client.conf
[oc-rsync]
client  = yes
accept  = 127.0.0.1:8730
connect = daemon-host:874
verify  = 2
CAfile  = /etc/oc-rsync-ssl/ca.crt
```

Then connect to the local tunnel endpoint:

```sh
stunnel /etc/stunnel/oc-rsync-client.conf
oc-rsync -av rsync://localhost:8730/backups/ ./restore/
```

---

## Method 3: reverse proxy (recommended for production)

A reverse proxy is the production-recommended approach. It provides TLS
offload, certificate rotation via reload, health checking, connection limits,
and logging - all without touching the oc-rsync daemon configuration.

**Important:** the rsync wire protocol is not HTTP. The proxy must operate in
TCP/stream mode. An HTTP-mode proxy will corrupt the rsync protocol and
silently break transfers.

### HAProxy

```haproxy
# /etc/haproxy/haproxy.cfg
global
    log /dev/log local0
    maxconn 4096

defaults
    mode    tcp
    timeout connect 10s
    timeout client  1h
    timeout server  1h

listen oc-rsync-tls
    bind            *:874 ssl crt /etc/haproxy/certs/oc-rsync.pem
    mode            tcp
    option          tcplog
    server          oc-rsyncd 127.0.0.1:8730
```

Notes:

- `mode tcp` must appear in both `defaults` and `listen`. A stray `mode http`
  anywhere in the config will silently break connections.
- The `crt` path points to a PEM bundle containing the server certificate,
  private key, and any intermediate chain certificates, concatenated in that
  order.
- For mutual TLS, add `ca-file /etc/haproxy/ca.pem verify required` to the
  `bind` line.
- The long `timeout client` / `timeout server` (1 hour) accommodates large
  transfers. Adjust to match your workload.

Certificate rotation: `systemctl reload haproxy` or `haproxy -sf $(pidof haproxy)`
picks up new certificates without dropping active connections.

### nginx

nginx terminates TLS for TCP backends using the `stream {}` block. This block
is a top-level directive - a sibling of `http {}`, not nested inside it.

```nginx
# /etc/nginx/nginx.conf (or /etc/nginx/stream.d/oc-rsync.conf)
stream {
    upstream oc_rsyncd {
        server 127.0.0.1:8730;
    }

    server {
        listen              874 ssl;
        ssl_certificate     /etc/nginx/certs/oc-rsync.crt;
        ssl_certificate_key /etc/nginx/certs/oc-rsync.key;
        proxy_pass          oc_rsyncd;
        proxy_timeout       1h;
    }
}
```

For mutual TLS, add:

```nginx
ssl_client_certificate /etc/nginx/certs/ca.crt;
ssl_verify_client      on;
```

Certificate rotation: `nginx -s reload` picks up new certificates.

**Prerequisite:** nginx must be compiled with `--with-stream` and
`--with-stream_ssl_module`. Most distribution packages include these modules.

### Health checks

The rsync protocol begins with the daemon sending an `@RSYNCD:` greeting
immediately on connection. A bare TCP connect-and-disconnect health check
works fine. Do not use HTTP health checks - oc-rsyncd will receive `GET /` and
respond with the daemon greeting, which the proxy will interpret as a failed
check.

---

## Security considerations

### TLS wrapping is transport-layer only

TLS-in-front encrypts the bytes on the wire. It does not replace or
strengthen the daemon's own authentication. The `@RSYNCD:` AUTHREQD
challenge-response still runs inside the encrypted tunnel, using an MD5-based
hash of the password and a server-generated nonce.

This means:

- **TLS protects password hashes in transit.** Without TLS, the challenge-
  response exchange is visible to network sniffers.
- **The auth mechanism itself is MD5-based.** TLS does not upgrade it.
  For stronger authentication, layer mutual TLS (mTLS) on top so that
  only clients with a valid certificate can reach the daemon at all.

### Certificate verification matters

stunnel defaults to `verify = 0` - no certificate verification. This means
the TLS channel is encrypted but not authenticated: a man-in-the-middle can
present any certificate and the client will accept it.

For meaningful security:

- **Server-side:** set `verify = 2` (or higher) and provide a `CAfile` if you
  want to require client certificates (mTLS).
- **Client-side:** always verify the server certificate. Use `openssl s_client
  -verify_return_error -CAfile ...` or configure the client stunnel with
  `verify = 2`.
- **Production:** use certificates signed by a CA that both sides trust.
  Self-signed certificates are acceptable for testing but require manual
  distribution of the CA certificate to every client.

### Certificate pinning

For high-security deployments, pin the server certificate or its public key
on the client side. This prevents compromise of a trusted CA from allowing
impersonation:

```sh
# Extract the server's public key hash
openssl x509 -in server.crt -pubkey -noout | \
    openssl pkey -pubin -outform DER | \
    openssl dgst -sha256 -binary | \
    openssl enc -base64
```

Use the resulting hash in your TLS client configuration or verification
scripts.

### Bind the daemon to loopback

When a TLS terminator fronts the daemon, always set `address = 127.0.0.1` in
`oc-rsyncd.conf`. If the daemon also listens on a public interface, an
attacker can bypass TLS entirely by connecting to port 873 directly. The
per-module `hosts allow = 127.0.0.1` directive provides defense in depth.

### Firewall rules

Block external access to the cleartext daemon port (873 or your chosen backend
port) and allow only the TLS port (874) from the network:

```sh
# iptables example
iptables -A INPUT -p tcp --dport 873 -j DROP
iptables -A INPUT -p tcp --dport 874 -j ACCEPT
```

---

## Quick reference

| Scenario | Server setup | Client command |
|----------|--------------|----------------|
| stunnel server + openssl client | stunnel on port 874 | `oc-rsync --connect-program 'openssl s_client -quiet ...' rsync://host:874/mod/` |
| stunnel server + stunnel client | stunnel on port 874 | `oc-rsync rsync://localhost:8730/mod/` (local stunnel forwards) |
| HAProxy TLS termination | HAProxy on port 874 | Same as stunnel client options |
| nginx stream TLS | nginx on port 874 | Same as stunnel client options |
| rsync-ssl wrapper | Any TLS server on 874 | `rsync-ssl host::mod/` |

In all cases, set `RSYNC_PASSWORD` or pass `--password-file` for
authentication - the daemon's `auth users` challenge still runs inside the
TLS tunnel.

---

## See also

- [Daemon concurrency limits](daemon-concurrency-limits.md) - connection
  limits and thread-per-connection model.
- [Filter rules](filter-rules-status.md) - filter rule support status and
  known gaps.
