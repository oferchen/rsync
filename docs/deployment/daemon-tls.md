# Daemon TLS-in-Front Deployment Recipes

The rsync wire protocol does not negotiate TLS. `oc-rsync --daemon` exposes a plaintext TCP listener and authenticates clients with `auth users` + `secrets file` only. To run a public-facing daemon you must terminate TLS in a separate process and forward the decrypted stream to a loopback-bound daemon.

This document gives runnable recipes for the three supported deployment patterns:

1. `stunnel` in front of `oc-rsync --daemon` (recommended for most operators).
2. `ssh -L` ad-hoc tunnel for one-off client access.
3. `HAProxy` TCP-mode reverse proxy with TLS termination (recommended when you already operate HAProxy).

Each recipe assumes:

- The daemon binds only to `127.0.0.1:8730`.
- The TLS terminator owns port `873/tcp` (the standard `rsync://` port) and accepts external connections.
- A host firewall blocks any path to `127.0.0.1:8730` from outside the host.

For the security rationale and a list of related daemon hardening flags, see [`SECURITY.md`](../../SECURITY.md).

---

## 1. Shared daemon configuration

All three recipes use the same `oc-rsyncd.conf`. Save it as `/etc/oc-rsyncd/oc-rsyncd.conf`:

```ini
# /etc/oc-rsyncd/oc-rsyncd.conf
uid = nobody
gid = nogroup
use chroot = yes
max connections = 32
pid file = /run/oc-rsyncd/oc-rsyncd.pid
log file = /var/log/oc-rsyncd.log

# Loopback bind - only the TLS terminator may connect.
address = 127.0.0.1
port = 8730

# Refuse destructive options globally.
refuse options = delete *

[mirror]
    path = /srv/mirror
    comment = Public read-only mirror
    read only = yes
    numeric ids = yes
    hosts allow = 127.0.0.1
    hosts deny = *
    auth users = mirroruser
    secrets file = /etc/oc-rsyncd/secrets
```

Create the secrets file with mode `0600`:

```sh
sudo install -d -m 0750 -o root -g root /etc/oc-rsyncd
sudo install -m 0600 -o root -g root /dev/stdin /etc/oc-rsyncd/secrets <<'EOF'
mirroruser:replace-with-strong-shared-secret
EOF
```

Verify the daemon starts and binds only to loopback:

```sh
sudo -u nobody oc-rsync --daemon --no-detach --config=/etc/oc-rsyncd/oc-rsyncd.conf &
ss -tlnp | grep 8730   # expect 127.0.0.1:8730 only, never 0.0.0.0:8730
```

---

## 2. stunnel TLS terminator

`stunnel` is the simplest path for operators who do not already run a reverse proxy. It accepts TLS on `873/tcp` and forwards plaintext to `127.0.0.1:8730`.

### 2.1 stunnel configuration

Save as `/etc/stunnel/oc-rsync.conf`:

```ini
# /etc/stunnel/oc-rsync.conf
foreground = yes
output = /var/log/stunnel/oc-rsync.log
pid = /run/stunnel/oc-rsync.pid
setuid = stunnel
setgid = stunnel

# Strong defaults; override per your TLS policy.
sslVersionMin = TLSv1.2
ciphers = HIGH:!aNULL:!MD5:!3DES:!RC4
options = NO_SSLv2
options = NO_SSLv3
options = NO_TLSv1
options = NO_TLSv1.1

[oc-rsync-tls]
    accept  = 0.0.0.0:873
    connect = 127.0.0.1:8730
    cert    = /etc/stunnel/certs/rsync.pem
    key     = /etc/stunnel/certs/rsync.key
    # Uncomment to require client certificates (best-effort mTLS at the
    # transport layer; oc-rsync still uses its own auth users+secrets).
    # CAfile = /etc/stunnel/certs/clients-ca.pem
    # verifyChain = yes
    # verifyPeer  = yes
```

Provision the certificate (`/etc/stunnel/certs/rsync.pem` for the public chain, `/etc/stunnel/certs/rsync.key` mode `0600` for the private key) with your usual ACME client or internal CA.

### 2.2 Client invocation

Native rsync clients still speak plaintext, so the connection must be wrapped client-side. The standard wrapper is `stunnel` in client mode, or `socat`:

```sh
# Option A: socat one-shot, then point oc-rsync at the local forwarder.
socat TCP-LISTEN:1873,bind=127.0.0.1,reuseaddr,fork \
      OPENSSL:mirror.example.com:873,verify=1 &
oc-rsync -av rsync://mirroruser@127.0.0.1:1873/mirror/ ./local/
```

```sh
# Option B: client-side stunnel.
cat >/etc/stunnel/oc-rsync-client.conf <<'EOF'
client = yes
foreground = yes
[oc-rsync]
accept  = 127.0.0.1:1873
connect = mirror.example.com:873
verifyChain = yes
CApath  = /etc/ssl/certs
EOF
stunnel /etc/stunnel/oc-rsync-client.conf &
oc-rsync -av rsync://mirroruser@127.0.0.1:1873/mirror/ ./local/
```

---

## 3. ssh -L ad-hoc tunnel

For ad-hoc access where the operator already has an SSH account on the daemon host, an `ssh -L` tunnel is the lowest-friction option. It does not require a TLS certificate, key management, or stunnel.

```sh
# Client side: forward local 8730 to the daemon-host loopback.
ssh -N -L 8730:127.0.0.1:8730 mirror.example.com
```

In a separate terminal on the client:

```sh
oc-rsync -av rsync://mirroruser@localhost:8730/mirror/ ./local/
```

The forwarded socket is bound to `127.0.0.1` on the client by default. Add `-L 127.0.0.1:8730:127.0.0.1:8730` explicitly if your `~/.ssh/config` has `GatewayPorts yes`.

This pattern is intended for occasional administrative access. For unattended automation (mirror jobs, backup pulls), use stunnel or HAProxy.

---

## 4. HAProxy TCP-mode reverse proxy

If you already operate HAProxy you can terminate TLS there and forward plaintext to the daemon. HAProxy in TCP mode does not parse the rsync protocol; it only relays bytes after the TLS handshake completes.

Save as `/etc/haproxy/haproxy.cfg` (excerpt):

```haproxy
global
    log /dev/log local0
    user  haproxy
    group haproxy
    maxconn 4096
    ssl-default-bind-options ssl-min-ver TLSv1.2 no-tls-tickets
    ssl-default-bind-ciphers HIGH:!aNULL:!MD5:!3DES:!RC4

defaults
    log     global
    mode    tcp
    option  tcplog
    timeout connect 10s
    timeout client  1h
    timeout server  1h

frontend rsync_tls
    bind :873 ssl crt /etc/haproxy/certs/rsync.pem
    mode tcp
    option tcplog
    default_backend rsync_local

backend rsync_local
    mode tcp
    server oc-rsyncd 127.0.0.1:8730 check
```

Place the combined PEM (private key + leaf + chain) at `/etc/haproxy/certs/rsync.pem` with mode `0600`. The client invocation is the same as the stunnel recipe (`socat OPENSSL:` or client-side `stunnel`).

To propagate the original client IP to the daemon log, add `send-proxy-v2` to the `server` line and run a PROXY-v2-aware terminator in front of the daemon. `oc-rsync --daemon` does not parse PROXY protocol natively (see "Caveats" below).

---

## 5. systemd unit excerpts

### 5.1 `oc-rsync-daemon.service`

Run the daemon as a hardened service bound to loopback.

```ini
# /etc/systemd/system/oc-rsync-daemon.service
[Unit]
Description=oc-rsync daemon (loopback only)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nobody
Group=nogroup
ExecStart=/usr/bin/oc-rsync --daemon --no-detach --config=/etc/oc-rsyncd/oc-rsyncd.conf
Restart=on-failure
RestartSec=2

# Hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
ReadWritePaths=/var/log /run/oc-rsyncd
RuntimeDirectory=oc-rsyncd

[Install]
WantedBy=multi-user.target
```

### 5.2 `stunnel.service` drop-in

```ini
# /etc/systemd/system/oc-rsync-stunnel.service
[Unit]
Description=stunnel TLS terminator for oc-rsync
After=oc-rsync-daemon.service network-online.target
Requires=oc-rsync-daemon.service

[Service]
Type=simple
User=stunnel
Group=stunnel
ExecStart=/usr/bin/stunnel /etc/stunnel/oc-rsync.conf
Restart=on-failure
RestartSec=2

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/stunnel /run/stunnel
RuntimeDirectory=stunnel
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

### 5.3 `haproxy.service` drop-in

```ini
# /etc/systemd/system/oc-rsync-haproxy.service
[Unit]
Description=HAProxy TLS terminator for oc-rsync
After=oc-rsync-daemon.service network-online.target
Requires=oc-rsync-daemon.service

[Service]
Type=notify
User=haproxy
Group=haproxy
ExecStart=/usr/sbin/haproxy -W -db -f /etc/haproxy/haproxy.cfg
ExecReload=/usr/sbin/haproxy -c -f /etc/haproxy/haproxy.cfg
KillMode=mixed
Restart=on-failure
RestartSec=2

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log /run/haproxy
RuntimeDirectory=haproxy
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

Enable the pair you choose:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now oc-rsync-daemon.service
sudo systemctl enable --now oc-rsync-stunnel.service        # stunnel deployments
# or
sudo systemctl enable --now oc-rsync-haproxy.service        # haproxy deployments
```

---

## 6. Firewall guidance

Only the TLS terminator's listening port (`873/tcp`) should be exposed externally. Block all external traffic to the daemon's loopback port.

### 6.1 nftables

```nft
# /etc/nftables.d/oc-rsync.nft
table inet filter {
    chain input {
        type filter hook input priority 0; policy drop;

        iif lo accept
        ct state established,related accept

        # Allow only the TLS terminator's port from the network.
        tcp dport 873 accept

        # Explicitly deny external traffic to the daemon's loopback port.
        # (Redundant with policy drop, but kept for audit clarity.)
        iif != lo tcp dport 8730 drop
    }
}
```

Apply with `sudo nft -f /etc/nftables.d/oc-rsync.nft`.

### 6.2 iptables (legacy)

```sh
# Default-deny inbound, accept loopback and established flows.
iptables -P INPUT DROP
iptables -A INPUT -i lo -j ACCEPT
iptables -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

# Public TLS port.
iptables -A INPUT -p tcp --dport 873 -j ACCEPT

# Belt-and-braces: drop any non-loopback traffic to the daemon port.
iptables -A INPUT -p tcp --dport 8730 ! -i lo -j DROP
```

Persist with `iptables-save > /etc/iptables/rules.v4` (or your distribution's equivalent).

The daemon's `address = 127.0.0.1` directive already binds only to loopback, so in a correctly configured host the firewall rule for port `8730` is redundant. The rule exists to fail closed if a misconfiguration removes that bind.

---

## 7. What these recipes do NOT do

These recipes intentionally have a small scope. They do not:

- **Provide native mTLS at the rsync layer.** `oc-rsync --daemon` authenticates clients only via `auth users` + `secrets file` (challenge-response over the rsync protocol). Adding `verifyChain = yes` + `CAfile` to stunnel, or `verify required ca-file ...` to HAProxy, gives you mutual TLS at the transport layer; the rsync challenge-response still runs underneath. Treat the rsync secret as a second factor, not as the only credential.
- **Forward the original client IP to `oc-rsync --daemon` by default.** With the configurations above, every client appears in `oc-rsyncd.log` and in the `hosts allow`/`hosts deny` evaluation as `127.0.0.1`. If you need original-IP propagation, you have two options, both of which require additional moving parts:
  - stunnel `transparent = source` (Linux only, requires `CAP_NET_ADMIN`, the right routing table, and kernel `TPROXY` support). The daemon then sees the real client address but does not authenticate it cryptographically.
  - HAProxy `server ... send-proxy-v2` plus a PROXY-v2-aware terminator immediately in front of the daemon. `oc-rsync --daemon` does not parse PROXY protocol natively; you would need a small shim (for example, a second stunnel instance with `protocol = proxy`) between HAProxy and the daemon.
- **Replace `auth users` / `secrets file`.** TLS protects bytes on the wire; the rsync layer still decides who may read or write modules. Keep `secrets file` mode `0600` and prefer `read only = yes` modules where possible.
- **Cover OpenVPN / WireGuard tunnels.** A site-to-site VPN is a valid alternative to TLS termination, but the configuration is out of scope for this document.

For the broader hardening checklist (chroot, `refuse options`, `numeric ids`, `hosts allow`/`hosts deny`, secrets file modes), see [`SECURITY.md`](../../SECURITY.md).
