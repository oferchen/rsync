# TLS in Front of the oc-rsync Daemon

oc-rsync's daemon mode speaks the plaintext rsync wire protocol over TCP,
just like upstream rsync. The daemon authenticates clients itself
(`auth users` + `secrets file`), but the bytes on the wire are not
encrypted. Operators that want network-level confidentiality and
integrity put a TLS terminator in front of the daemon.

This page documents three sidecar deployments: **stunnel**, **SSH
tunnel**, and a **TCP-mode reverse proxy** (HAProxy or nginx). All three
work with an unmodified oc-rsyncd; oc-rsync ships no native TLS client
or server.

**Last verified:** 2026-05-01 against
`crates/daemon/src/daemon/sections/config_parsing/global_directives.rs`,
`crates/daemon/src/daemon/sections/config_parsing/module_directives.rs`,
`crates/cli/src/frontend/command_builder/sections/build_base_command/network.rs`,
and upstream `rsync-3.4.1/stunnel-rsyncd.conf`.

---

## Threat model: what TLS-in-front does and does not add

TLS-in-front is a wire-level encryption wrapper. The terminator accepts
TLS from the network, decrypts it, and forwards plain rsync protocol to
the loopback-bound oc-rsyncd. From the daemon's perspective the
connection is a local TCP session.

| Property | Provided by oc-rsyncd | Provided by TLS-in-front |
|----------|-----------------------|--------------------------|
| Confidentiality on the wire | No | Yes |
| Integrity on the wire | Protocol checksums only | Yes (TLS MAC) |
| Server identity proof to client | No | Yes (server cert) |
| Client identity proof to server | `auth users` (rsync challenge) | Optional (mTLS) |
| Module-level access control | Yes (`auth users`, `hosts allow`) | No |
| Filter rules / read-only enforcement | Yes | No |
| Path-traversal / chroot enforcement | Yes | No |

TLS-in-front does **not** replace daemon authentication. The
`@RSYNCD:` AUTHREQD challenge and `secrets file` check still run inside
the TLS tunnel. Treat TLS as transport hardening on top of, not instead
of, the daemon's own access controls.

---

## stunnel

stunnel is the upstream-blessed pattern. The upstream tarball ships
`stunnel-rsyncd.conf`. Convention is to listen on TCP/874 (the
`rsync-ssl` IANA-style port; not formally registered) and forward to
`localhost:873`.

### `/etc/stunnel/oc-rsyncd.conf`

```ini
foreground = no
pid = /var/run/stunnel-oc-rsyncd.pid
socket = l:TCP_NODELAY=1
socket = r:TCP_NODELAY=1

# stunnel itself runs as root only because rsync's chroot needs it;
# oc-rsyncd drops privileges after binding.
setuid = root
setgid = root

[oc-rsync]
accept  = 874
connect = 127.0.0.1:8730
cert    = /etc/oc-rsync-ssl/server.crt
key     = /etc/oc-rsync-ssl/server.key
client  = no

# Open access. Replace with verify = 2 plus CAfile for mTLS.
verify  = 0
CAfile  = /etc/ssl/certs/ca-certificates.crt
```

### `/etc/oc-rsyncd.conf` (matching daemon)

```ini
# Bind to loopback only - the only path in is via stunnel.
address = 127.0.0.1
port    = 8730

[backups]
    path         = /srv/backups
    read only    = false
    auth users   = backup-bot
    secrets file = /etc/oc-rsyncd.secrets
    hosts allow  = 127.0.0.1
```

The `address` directive is upstream's name for the bind address (alias
of `bind_address` in `daemon-parm.txt`). `port` overrides the default
873. Both are honored in the daemon's global section.

### Client invocation

stunnel makes the daemon look like a daemon on TCP/874 to the client.
oc-rsync has no native TLS client, so the client speaks plain rsync to
its own local stunnel acting in client mode (or to any TLS-aware tunnel
on the client host):

```sh
# Client-side stunnel forwards 127.0.0.1:8730 -> server:874 over TLS.
oc-rsync -av rsync://localhost:8730/backups/ ./restore/
```

The upstream `rsync-ssl` wrapper script does the same thing by spawning
`openssl s_client` as the connect program. oc-rsync supports the same
mechanism via `--connect-program`; supplying an `openssl s_client` or
`stunnel -c` invocation there avoids running a long-lived client-side
sidecar.

---

## SSH tunnel

A user-mode SSH local-forward gives you authenticated, encrypted
transport with no extra software. This is **not** the same as
rsync-over-ssh transport (`oc-rsync -e ssh ...`):

- **rsync-over-ssh (`-e ssh`)** spawns a remote `oc-rsync --server`
  process over SSH stdio. There is no daemon involved on the far side.
- **SSH tunnel to a daemon** keeps oc-rsyncd running on the far side
  and tunnels the daemon's TCP socket through SSH. The far side still
  parses `oc-rsyncd.conf`, enforces `auth users`, and exposes named
  modules.

Use the SSH-tunnel pattern when you want daemon semantics (modules,
`read only`, `auth users`, `pre-xfer exec`) but not a separate TLS
terminator.

### Server side

```ini
# /etc/oc-rsyncd.conf on the daemon host
address = 127.0.0.1
port    = 8730

[backups]
    path       = /srv/backups
    read only  = true
    hosts allow = 127.0.0.1
```

### Client side

```sh
# Open the tunnel in one terminal (or as a systemd unit).
ssh -N -L 8730:localhost:8730 daemon-host

# In another terminal, talk to the local end of the tunnel.
oc-rsync -av rsync://localhost:8730/backups/ ./restore/
```

Equivalent one-shot form using `--port`:

```sh
ssh -N -L 9999:localhost:8730 daemon-host &
oc-rsync -av --port 9999 rsync://localhost/backups/ ./restore/
```

`--port` overrides the default rsync:// port for URLs that omit it. See
`crates/cli/src/frontend/command_builder/sections/build_base_command/network.rs`.

---

## Reverse proxy (HAProxy or nginx) in TCP mode

HAProxy and nginx can both terminate TLS for arbitrary TCP backends.
**They must be configured in TCP / stream mode, not HTTP mode.** The
rsync wire protocol is not HTTP and will be corrupted by any L7 HTTP
proxy.

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

- `mode tcp` everywhere. A stray `mode http` in `defaults` will break
  the connection silently.
- `crt` points at a PEM bundle (server cert + private key + chain).
- For mTLS add `ca-file /etc/haproxy/ca.pem verify required` to the
  `bind` line.

### nginx

nginx terminates TLS for TCP backends with `stream {}`, which lives at
the top level (sibling of `http {}`), not inside it.

```nginx
# /etc/nginx/nginx.conf
stream {
    upstream oc_rsyncd {
        server 127.0.0.1:8730;
    }

    server {
        listen           874 ssl;
        ssl_certificate  /etc/nginx/certs/oc-rsync.crt;
        ssl_certificate_key /etc/nginx/certs/oc-rsync.key;
        proxy_pass       oc_rsyncd;
        proxy_timeout    1h;
    }
}
```

For mTLS add `ssl_client_certificate /etc/nginx/ca.crt;` and
`ssl_verify_client on;`.

---

## Connecting from the client

oc-rsync has no native TLS client. Whatever TLS terminator you deploy on
the server, the client must terminate TLS before handing bytes to
`oc-rsync`. Three common shapes:

1. **Local stunnel client.** Run `stunnel` on the client host with
   `client = yes` and `accept = 8730`, `connect = server:874`. Then run
   `oc-rsync rsync://localhost:8730/module/`.

2. **`--connect-program` with `openssl s_client`.** The CLI accepts
   `--connect-program COMMAND` to launch a custom transport for
   `rsync://` URLs (placeholders `%H` for host, `%P` for port). This
   matches upstream's `rsync-ssl` wrapper. Example:

   ```sh
   oc-rsync \
       --connect-program 'openssl s_client -quiet -verify_quiet -servername %H -connect %H:%P' \
       -av rsync://daemon-host:874/backups/ ./restore/
   ```

3. **SSH local-forward.** As above. The TLS layer is replaced by SSH's
   own encryption. Client just speaks plain rsync to the loopback end.

In all three cases the daemon-side `auth users` challenge still runs
inside the encrypted tunnel. Set `RSYNC_PASSWORD` or supply
`--password-file` exactly as for an unwrapped daemon.

---

## Operational notes

### Certificates

- Server certs need a SAN matching the hostname clients use. Browsers
  and TLS libraries reject the legacy CN-only form. Generate with
  `openssl req -addext "subjectAltName=DNS:rsync.example.com"`.
- For internal deployments, a private CA + DNS-SAN cert is the cleanest
  story. Public CA via ACME works the same; nothing in the rsync
  protocol cares.
- Rotate by reloading the terminator (`systemctl reload stunnel`,
  `haproxy -sf`, `nginx -s reload`). oc-rsyncd does not need to know
  about cert changes.

### mTLS

All three terminators support requiring a client certificate:

| Terminator | Directive |
|------------|-----------|
| stunnel    | `verify = 2` (or `3` / `4`) plus `CAfile` |
| HAProxy    | `bind ... ssl crt ... ca-file ... verify required` |
| nginx      | `ssl_client_certificate` + `ssl_verify_client on` |

mTLS is in addition to, not instead of, daemon `auth users`. Both
checks run; both must pass.

### Binding the daemon

Always set `address = 127.0.0.1` (or a private VLAN address) when a TLS
terminator fronts oc-rsyncd. If the daemon also listens on a public
interface, an attacker can bypass TLS by talking to it directly.
`hosts allow = 127.0.0.1` (per-module) is a defense in depth.

`port` defaults to 873. Pick anything in the unprivileged range above
1024 for the loopback listener (this page uses 8730) so the daemon does
not need to keep CAP_NET_BIND_SERVICE for the backend port.

### Client `--port`

`oc-rsync --port PORT` overrides the default 873 for `rsync://` URLs
that do not embed a port. URLs of the form
`rsync://host:PORT/module/` always win over `--port`.

### Health checks

Reverse proxies often add TCP health checks. The rsync protocol begins
with the daemon sending an `@RSYNCD:` greeting unprompted. A bare TCP
connect-and-close health check is fine. Do not enable HTTP health
checks: oc-rsyncd will receive `GET /` and respond with the daemon
greeting, which the proxy will then mark as a failure.

---

## See also

- [`docs/daemon/filter-precedence.md`](filter-precedence.md) - how
  `oc-rsyncd.conf` filter rules combine with client-supplied filters.
- [`docs/DAEMON_PROCESS_MODEL.md`](../DAEMON_PROCESS_MODEL.md) -
  oc-rsyncd's thread / task model vs upstream's fork-per-connection.
- Upstream reference: `target/interop/upstream-src/rsync-3.4.1/stunnel-rsyncd.conf`
  and `rsyncd.conf.5.md`.
