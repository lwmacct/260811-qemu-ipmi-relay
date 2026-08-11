# Deployment

The relay listens on a Unix stream socket. QEMU connects to that socket and
exposes the external BMC through an emulated ISA KCS interface.

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

Install the binary, service unit, and one configuration file from the extracted
directory:

```sh
install -Dm0755 bin/qemu-ipmi-relay /usr/local/sbin/qemu-ipmi-relay
install -Dm0644 systemd/qemu-ipmi-relay@.service \
  /etc/systemd/system/qemu-ipmi-relay@.service
install -Dm0640 config/example.toml \
  /etc/qemu-ipmi-relay/example-vm.toml
systemctl daemon-reload
systemctl enable --now qemu-ipmi-relay@example-vm.service
```

The service runs as root because the host currently exposes `/dev/ipmi0` as
mode `0600`. Its primary group is `incus`, and the relay creates its socket as
mode `0660`, allowing the Incus QEMU process to connect. Adjust the service
group if QEMU runs under a different account.

Keep the socket outside `/run/incus/<instance>/`. Incus recreates that
instance runtime directory during VM startup and may remove a relay socket
placed there. The service's `/run/qemu-ipmi-relay/` runtime directory is
independent of the instance lifecycle.

## QEMU devices

The equivalent QEMU command-line fragment is:

```text
-chardev socket,id=ipmi-relay,path=/run/qemu-ipmi-relay/example-vm.sock,reconnect-ms=1000 \
-device ipmi-bmc-extern,id=host-bmc,chardev=ipmi-relay \
-device isa-ipmi-kcs,bmc=host-bmc
```

For Incus, [`config/raw.qemu.conf`](../config/raw.qemu.conf) contains the
complete value for the VM's `raw.qemu.conf` setting. Apply that file while the
VM is stopped:

```sh
incus stop example-vm
incus config set example-vm "raw.qemu.conf=$(<config/raw.qemu.conf)"
incus start example-vm
```

Incus confinement must allow QEMU to connect to the selected Unix socket.
Confirm this on the target host by checking the instance log and the Incus
daemon log after the first start. Do not weaken confinement globally.
The `reconnect-ms` setting lets QEMU reconnect after the relay service restarts.

## Guest validation

Load the normal Linux IPMI drivers and verify the virtual device:

```sh
modprobe ipmi_si
modprobe ipmi_devintf
ls -l /dev/ipmi0
ipmitool mc info
ipmitool lan print
```

The relay does not filter commands. Every command issued by the guest is sent
to the physical BMC, including chassis power, BMC reset, user-management, and
configuration-changing commands.
