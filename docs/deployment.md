# Deployment

The relay accepts multiple QEMU connections on one systemd-managed Unix stream
socket and exposes the physical BMC to each VM through an emulated ISA KCS
interface. A single OpenIPMI worker serializes transactions to `/dev/ipmi0`.

## Host service

Pull the published AMD64 artifact and verify it:

```sh
mkdir qemu-ipmi-relay-release
oras pull \
  --output qemu-ipmi-relay-release \
  ghcr.io/lwmacct/260811-qemu-ipmi-relay:artifact-amd64-latest
(cd qemu-ipmi-relay-release && sha256sum --check SHA256SUMS)
tar -xzf qemu-ipmi-relay-release/qemu-ipmi-relay-linux-amd64.tar.gz
```

Install the binary, systemd units, and global configuration from the extracted
directory:

```sh
install -Dm0755 bin/qemu-ipmi-relay /usr/local/sbin/qemu-ipmi-relay
install -Dm0644 systemd/qemu-ipmi-relay.service \
  /etc/systemd/system/qemu-ipmi-relay.service
install -Dm0644 systemd/qemu-ipmi-relay.socket \
  /etc/systemd/system/qemu-ipmi-relay.socket
install -Dm0640 config/example.toml \
  /etc/qemu-ipmi-relay/config.toml
systemctl daemon-reload
systemctl enable --now qemu-ipmi-relay.socket
```

The service runs as root because the host may expose `/dev/ipmi0` as mode
`0600`. The socket unit creates `/run/qemu-ipmi-relay/ipmi.sock` as
`root:incus` mode `0660`, allowing Incus QEMU processes to connect. Adjust
`SocketGroup` if QEMU runs under a different account. The service starts on
the first connection. The socket is ordered before `incus.service`, and the
relay retries the configured OpenIPMI device until `device_wait_timeout_ms`
expires. If the timeout expires, systemd restarts the service and begins
another wait cycle while the listening socket remains available.

Keep the socket outside `/run/incus/<instance>/`. Incus recreates that
instance runtime directory during VM startup and may remove a relay socket
placed there. The socket unit's `/run/qemu-ipmi-relay/` directory is independent
of every instance lifecycle.

## QEMU devices

The equivalent QEMU command-line fragment is:

```text
-chardev socket,id=ipmi-relay,path=/run/qemu-ipmi-relay/ipmi.sock,reconnect-ms=1000 \
-device ipmi-bmc-extern,id=host-bmc,chardev=ipmi-relay \
-device isa-ipmi-kcs,bmc=host-bmc
```

For Incus, [`config/raw.qemu.conf`](../config/raw.qemu.conf) contains the
complete value for each VM's `raw.qemu.conf` setting. Apply that same file to
every VM while it is stopped:

```sh
incus stop example-vm
incus config set example-vm "raw.qemu.conf=$(<config/raw.qemu.conf)"
incus config set example-vm boot.autostart=true
incus start example-vm
```

Incus confinement must allow QEMU to connect to the selected Unix socket.
Confirm this on the target host by checking the instance log and the Incus
daemon log after the first start. Do not weaken confinement globally. The
`reconnect-ms` setting retries temporary connection failures, but it does not
make an established VM connection restart-safe on every QEMU version. On the
validated Incus/QEMU target, restarting the relay caused connected QEMU
processes to exit. Stop all attached VMs before upgrading or restarting the
relay service.
All connected VMs operate on the same physical BMC. All requests are
serialized; configuration, reset, and power commands issued by any VM affect
the same hardware.

## Guest validation

Load the normal Linux IPMI drivers and verify the virtual device:

```sh
modprobe ipmi_si
modprobe ipmi_devintf
until test -c /dev/ipmi0; do sleep 1; done
ls -l /dev/ipmi0
ipmitool mc info
ipmitool lan print
```

The relay does not filter commands. Every command issued by the guest is sent
to the physical BMC, including chassis power, BMC reset, user-management, and
configuration-changing commands.
