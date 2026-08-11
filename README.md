# 260811-qemu-ipmi-relay

Transparent relay between QEMU's virtual VM-IPMI/KCS interface and the Linux
OpenIPMI system interface. The relay understands only framing and the Linux
ioctl ABI; it does not inspect or filter IPMI command semantics.

## Status

The protocol codec, OpenIPMI backend, single-connection relay, tests, and
systemd packaging are implemented. Validation with a real QEMU KCS device and
physical BMC is still required before the first production release.

## Data path

```text
guest ipmitool -> guest /dev/ipmi0 -> QEMU isa-ipmi-kcs
  -> QEMU VM-IPMI framing -> Unix socket -> qemu-ipmi-relay
  -> Linux OpenIPMI ioctl -> host /dev/ipmi0 -> physical BMC
```

Only transport framing is decoded: sequence, NetFn/LUN, command, payload,
checksum, escaping, and frame boundaries. Command meaning is neither inspected
nor changed. There is no allowlist or denylist.

## Configuration

```toml
socket = "/run/qemu-ipmi-relay/ipmi.sock"
device = "/dev/ipmi0"
request_timeout_ms = 3000
max_frame_size = 303
```

Run with defaults or an explicit configuration file:

```sh
cargo run -- --config config/example.toml
```

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
