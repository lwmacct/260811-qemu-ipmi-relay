# 260811-qemu-ipmi-relay

Transparent relay between QEMU's virtual VM-IPMI/KCS interface and the Linux
OpenIPMI system interface. The relay understands only framing and the Linux
ioctl ABI; it does not inspect or filter IPMI command semantics.

## Status

The protocol codec, OpenIPMI backend, multi-client relay, tests, and systemd
socket-activation packaging are implemented. End-to-end validation has been
completed with two concurrent QEMU KCS VMs and a physical BMC: guest
`ipmitool mc info` and `ipmitool lan print` succeeded from both VMs and matched
the host BMC data.

## Data path

```text
multiple guests -> QEMU isa-ipmi-kcs -> shared Unix socket
  -> per-connection relay workers -> bounded request queue
  -> single OpenIPMI worker -> host /dev/ipmi0 -> physical BMC
```

Only transport framing is decoded: sequence, NetFn/LUN, command, payload,
checksum, escaping, and frame boundaries. Command meaning is neither inspected
nor changed. There is no allowlist or denylist.

## Configuration

```toml
device = "/dev/ipmi0"
request_timeout_ms = 3000
max_frame_size = 303
max_connections = 128
queue_depth = 128
```

The listening socket is supplied through systemd socket activation. Every VM
connects to `/run/qemu-ipmi-relay/ipmi.sock`; the relay accepts the connections
concurrently and serializes physical BMC transactions through one OpenIPMI
worker.

Set `RUST_LOG=debug` to log each forwarded request's sequence, NetFn/LUN, and
command number. Payload bytes are not logged.

See [docs/deployment.md](docs/deployment.md) for systemd and QEMU/Incus wiring.

## Published AMD64 artifact

Successful builds on `main` publish a Linux AMD64 release to GHCR as a generic
OCI artifact:

```text
ghcr.io/lwmacct/260811-qemu-ipmi-relay:artifact-amd64-latest
ghcr.io/lwmacct/260811-qemu-ipmi-relay:artifact-amd64-sha-<commit-id-12>
```

Pull and verify it with ORAS:

```sh
mkdir qemu-ipmi-relay-release
oras pull \
  --output qemu-ipmi-relay-release \
  ghcr.io/lwmacct/260811-qemu-ipmi-relay:artifact-amd64-latest
(cd qemu-ipmi-relay-release && sha256sum --check SHA256SUMS)
```

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```
