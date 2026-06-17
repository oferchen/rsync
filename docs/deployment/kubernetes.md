# Kubernetes Deployment

This guide covers running oc-rsync as a workload inside Kubernetes Pods.
The focus is the interaction between Pod `securityContext`, the
`CAP_SYS_NICE` capability that `IORING_SETUP_SQPOLL` requires, and the
`--no-io-uring-sqpoll` CLI flag that lets operators opt out of SQPOLL
without disabling io_uring entirely.

For the broader container guide (Podman, Docker, kernel matrix), see
[container-io-uring.md](container-io-uring.md); the
[SQPOLL in rootless containers](container-io-uring.md#sqpoll-in-rootless-containers)
section there covers the rootless detection helper, fallback log, and
throughput delta that apply equally to Pods. For the per-feature
kernel requirement matrix, see
[io-uring-feature-matrix.md](io-uring-feature-matrix.md).

## Contents

- [1. Quickstart: rsync transfer as a one-shot Job](#1-quickstart-rsync-transfer-as-a-one-shot-job)
- [2. SQPOLL and CAP_SYS_NICE inside Pods](#2-sqpoll-and-cap_sys_nice-inside-pods)
- [3. Rootless Pods and the `--no-io-uring-sqpoll` opt-out](#3-rootless-pods-and-the---no-io-uring-sqpoll-opt-out)
- [4. Daemon-as-Pod deployment](#4-daemon-as-pod-deployment)
- [5. Pod Security Standards / Pod Security Admission](#5-pod-security-standards--pod-security-admission)
- [6. Verifying the active io_uring tier](#6-verifying-the-active-io_uring-tier)
- [7. Troubleshooting](#7-troubleshooting)

## 1. Quickstart: rsync transfer as a one-shot Job

The simplest deployment is a `Job` that copies between two PVCs and
exits. No extra capabilities are required; oc-rsync falls back to
standard buffered I/O if io_uring is unavailable.

```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: rsync-copy
spec:
  backoffLimit: 0
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: rsync
          image: ghcr.io/example/oc-rsync:latest
          args:
            - "-aHAX"
            - "--no-io-uring-sqpoll"
            - "/source/"
            - "/dest/"
          volumeMounts:
            - name: source
              mountPath: /source
            - name: dest
              mountPath: /dest
      volumes:
        - name: source
          persistentVolumeClaim:
            claimName: source-pvc
        - name: dest
          persistentVolumeClaim:
            claimName: dest-pvc
```

`--no-io-uring-sqpoll` is the right default for rootless / restricted
Pods; see [section 3](#3-rootless-pods-and-the---no-io-uring-sqpoll-opt-out)
for the rationale.

## 2. SQPOLL and CAP_SYS_NICE inside Pods

`IORING_SETUP_SQPOLL` spawns a kernel thread that polls the submission
queue, removing one `io_uring_enter(2)` syscall per batch. The kernel
requires `CAP_SYS_NICE` (or root) to grant the elevated scheduling
priority that thread runs at. Inside a Pod that means:

- The container must run with `SYS_NICE` in
  `securityContext.capabilities.add`.
- The container must NOT be confined by a seccomp profile that drops
  `io_uring_setup(2)`.
- The cluster's admission controller (Pod Security Admission, OPA
  Gatekeeper, Kyverno) must allow `SYS_NICE` to be added.

Manifest snippet for a Pod that explicitly grants `CAP_SYS_NICE`:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: oc-rsync-sqpoll
spec:
  containers:
    - name: rsync
      image: ghcr.io/example/oc-rsync:latest
      args: ["--io-uring", "src/", "dst/"]
      securityContext:
        capabilities:
          add:
            - SYS_NICE
```

The graceful fallback path still applies: if the kernel rejects SQPOLL
despite the capability (for example because the seccomp profile blocks
`io_uring_setup`), oc-rsync silently builds a regular io_uring ring and
sets the diagnostic flag visible via `--io-uring-status`.

## 3. Rootless Pods and the `--no-io-uring-sqpoll` opt-out

Most production Kubernetes clusters run Pods rootless (no container
root, no extra Linux capabilities by default). In that mode:

- `securityContext.capabilities.add: ["SYS_NICE"]` is rejected by the
  cluster's Pod Security Admission policy.
- Without `SYS_NICE`, `IORING_SETUP_SQPOLL` returns `EPERM` and
  oc-rsync transparently builds a regular ring.
- Some seccomp profiles block `io_uring_setup` entirely, in which case
  oc-rsync falls back to standard buffered I/O.

There is a third option for clusters that disallow `SYS_NICE` but still
want io_uring acceleration:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: oc-rsync-no-sqpoll
spec:
  containers:
    - name: rsync
      image: ghcr.io/example/oc-rsync:latest
      args:
        - "--no-io-uring-sqpoll"
        - "src/"
        - "dst/"
      # No extra capabilities required.
```

`--no-io-uring-sqpoll` keeps io_uring on for file and socket I/O,
including BGID-based buffer rings, registered buffers, file
registration, and zero-copy socket sends when supported. Only the
`IORING_SETUP_SQPOLL` flag is suppressed. This makes the runtime
behaviour identical to what you would observe in a rootless Pod
**before** the EPERM fallback kicks in: the kthread is never requested
in the first place.

### Why not just run as privileged?

Running `securityContext.privileged: true` grants every capability and
disables most isolation. It is the cluster equivalent of `--privileged`
on Podman/Docker and is strongly discouraged for production workloads.
The `--no-io-uring-sqpoll` flag gives the same throughput tradeoff
(lose SQPOLL, keep everything else) without weakening the security
posture.

### Tradeoffs vs `--cap-add SYS_NICE`

| Mode | Pod requirements | SQPOLL active | Other io_uring features | When to use |
|------|------------------|---------------|------------------------|-------------|
| `--io-uring` + `SYS_NICE` | `securityContext.capabilities.add: ["SYS_NICE"]` and a permissive PSA profile | Yes | Yes | Trusted clusters where you control the security profile and the workload is throughput-bound |
| `--no-io-uring-sqpoll` | None | No | Yes | Rootless Pods, baseline / restricted PSA profiles, audit-restricted clusters |
| `--io-uring` (default `Auto`) without `SYS_NICE` | None | Attempted, falls back on `EPERM` | Yes | Trusted but inconsistent cluster security profiles; relies on the kernel reject path |
| `--no-io-uring` | None | No | No (standard buffered I/O) | Debugging, container runtimes that block `io_uring_setup` entirely |

Quantitative numbers comparing rootless throughput with and without
SQPOLL on K8s are tracked separately and will be added once the
in-container benchmark harness lands.

## 4. Daemon-as-Pod deployment

Running the daemon as a Pod is supported when you want clients outside
the cluster to push or pull through `rsync://` URLs. The minimum
manifest is a `Deployment` with a `Service` that exposes the daemon's
listening port (873/tcp by default).

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: oc-rsyncd
  labels:
    app: oc-rsyncd
spec:
  replicas: 1
  selector:
    matchLabels:
      app: oc-rsyncd
  template:
    metadata:
      labels:
        app: oc-rsyncd
    spec:
      containers:
        - name: daemon
          image: ghcr.io/example/oc-rsync:latest
          command: ["oc-rsync"]
          args:
            - "--daemon"
            - "--no-detach"
            - "--config=/etc/oc-rsyncd.conf"
            - "--no-io-uring-sqpoll"
          ports:
            - name: rsync
              containerPort: 873
          volumeMounts:
            - name: config
              mountPath: /etc/oc-rsyncd.conf
              subPath: oc-rsyncd.conf
              readOnly: true
            - name: data
              mountPath: /srv/rsync
          securityContext:
            allowPrivilegeEscalation: false
            runAsNonRoot: true
            runAsUser: 65532
            capabilities:
              drop:
                - ALL
            seccompProfile:
              type: RuntimeDefault
      volumes:
        - name: config
          configMap:
            name: oc-rsyncd-config
        - name: data
          persistentVolumeClaim:
            claimName: rsync-data
---
apiVersion: v1
kind: Service
metadata:
  name: oc-rsyncd
spec:
  selector:
    app: oc-rsyncd
  ports:
    - name: rsync
      port: 873
      targetPort: 873
```

The daemon manifest pairs the rootless `securityContext` with
`--no-io-uring-sqpoll` so the io_uring fast path stays active without
requesting `CAP_SYS_NICE`. Add a `NetworkPolicy` to restrict ingress to
the client CIDR; external rsync over TLS belongs behind an stunnel
sidecar (see [`daemon-tls.md`](daemon-tls.md) for native TLS or the
external-terminator recipes).

## 5. Pod Security Standards / Pod Security Admission

Kubernetes 1.25+ ships Pod Security Admission (PSA) with three
profiles: `privileged`, `baseline`, and `restricted`. The interaction
with oc-rsync's io_uring features is:

| PSA Profile | `CAP_SYS_NICE` available | Default seccomp behaviour | Recommended flag |
|-------------|--------------------------|---------------------------|------------------|
| `restricted` | No (capabilities must be dropped) | `RuntimeDefault` profile permits `io_uring_setup` on most runtimes | `--no-io-uring-sqpoll` |
| `baseline` | Yes if requested in pod spec | `RuntimeDefault` permits `io_uring_setup` | `--io-uring` if `SYS_NICE` is added, else `--no-io-uring-sqpoll` |
| `privileged` | Yes (no admission constraint) | No seccomp filter | `--io-uring` |

For namespaces labelled `pod-security.kubernetes.io/enforce: restricted`
the pod spec MUST NOT include `securityContext.capabilities.add` -
admission rejects it. `--no-io-uring-sqpoll` is the correct choice
because it requires no capabilities.

OPA Gatekeeper, Kyverno, and similar policy engines apply their own
constraints. If your cluster blocks `SYS_NICE` cluster-wide via a
custom policy, the `--no-io-uring-sqpoll` flag is the only path to
io_uring acceleration short of relaxing the policy.

## 6. Verifying the active io_uring tier

After deploying, exec into the pod and run:

```bash
kubectl exec -it <pod> -- oc-rsync --io-uring-status
```

Example output inside a rootless Pod with `--no-io-uring-sqpoll`:

```text
io_uring capability matrix:

  compiled in:        yes
  platform:           linux
  kernel version:     6.1
  available:          yes
  supported ops:      48
  pbuf_ring:          yes (kernel 5.19+)
  sqpoll fell back:   no
  sqpoll opt-out:     yes (--no-io-uring-sqpoll)

  feature gates:
    io_uring:             on
    iouring-data-reads:   on
    iouring-send-zc:      on
```

`sqpoll fell back: no` together with `sqpoll opt-out: yes
(--no-io-uring-sqpoll)` is the expected outcome under the opt-out: the
kthread was never requested, so there is nothing to fall back from.
Contrast this with the default `Auto` policy in the same environment,
which would show `sqpoll fell back: yes (CAP_SYS_NICE likely missing)`
after the EPERM reject and `sqpoll opt-out: no`.

## 7. Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `Pod admission denied: capability "SYS_NICE" not allowed` | PSA `restricted` or custom policy | Drop `capabilities.add` and add `--no-io-uring-sqpoll` to the args |
| `--io-uring-status` reports `available: no` | seccomp profile blocks `io_uring_setup` | Use a runtime/profile that permits it, or accept the standard I/O fallback |
| `available: yes` but `sqpoll fell back: yes` | `Auto` policy and no `CAP_SYS_NICE` | Add `--no-io-uring-sqpoll` for a clean opt-out, or grant `SYS_NICE` |
| Daemon Pod fails liveness check on first connection | Read-only root filesystem prevents temp-file commit | Mount a writable `emptyDir` at `/tmp` or use `--inplace` |
| Throughput much lower than bare-metal benchmark | Standard I/O fallback engaged | Check the seccomp profile; if io_uring is genuinely unavailable, accept it |
