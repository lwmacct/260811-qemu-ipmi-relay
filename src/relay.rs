use crate::{
    error::Result,
    openipmi::Backend,
    vm_codec::{Decoder, Frame, encode_message},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};
use tracing::{debug, warn};

pub fn serve_connection<B: Backend>(
    mut stream: UnixStream,
    backend: &B,
    timeout: Duration,
    max_frame_size: usize,
) -> Result<()> {
    let mut decoder = Decoder::new(max_frame_size);
    let mut input = [0u8; 4096];
    loop {
        let count = stream.read(&mut input)?;
        if count == 0 {
            return Ok(());
        }
        for frame in decoder.push(&input[..count]) {
            match frame? {
                Frame::Message(request) => {
                    debug!(
                        sequence = request.sequence,
                        netfn_lun = request.netfn_lun,
                        command = request.command,
                        "forwarding IPMI request"
                    );
                    match backend.transact(&request, timeout) {
                        Ok(response) => stream.write_all(&encode_message(&response))?,
                        Err(error) => {
                            // Keep the chardev connected. QEMU owns the external-BMC
                            // timeout and will return IPMI_CC_TIMEOUT to the guest.
                            warn!(
                                sequence = request.sequence,
                                netfn_lun = request.netfn_lun,
                                command = request.command,
                                %error,
                                "OpenIPMI request failed; waiting for QEMU timeout"
                            );
                        }
                    }
                }
                Frame::Command(command) => {
                    warn!(?command, "ignoring QEMU IPMI control command");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        error::Result,
        vm_codec::{IpmiMessage, encode_message},
    };
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
    };

    struct EchoBackend;

    impl Backend for EchoBackend {
        fn transact(&self, request: &IpmiMessage, _timeout: Duration) -> Result<IpmiMessage> {
            Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: request.netfn_lun.wrapping_add(4),
                command: request.command,
                data: vec![0, request.data[0]],
            })
        }
    }

    struct FailOnceBackend {
        calls: AtomicUsize,
    }

    impl Backend for FailOnceBackend {
        fn transact(&self, request: &IpmiMessage, _timeout: Duration) -> Result<IpmiMessage> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(crate::error::RelayError::OpenIpmi("test failure".into()));
            }
            Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: request.netfn_lun.wrapping_add(4),
                command: request.command,
                data: vec![0],
            })
        }
    }

    #[test]
    fn relays_a_complete_message_without_interpreting_the_command() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || {
            serve_connection(server, &EchoBackend, Duration::from_secs(1), 303).unwrap();
        });
        let request = IpmiMessage {
            sequence: 9,
            netfn_lun: 0x18,
            command: 0xa0,
            data: vec![0xaa],
        };
        client.write_all(&encode_message(&request)).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(
            response,
            encode_message(&IpmiMessage {
                sequence: 9,
                netfn_lun: 0x1c,
                command: 0xa0,
                data: vec![0, 0xaa],
            })
        );
        handle.join().unwrap();
    }

    #[test]
    fn backend_failure_does_not_disconnect_qemu() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || {
            let backend = FailOnceBackend {
                calls: AtomicUsize::new(0),
            };
            serve_connection(server, &backend, Duration::from_secs(1), 303).unwrap();
        });
        let first = IpmiMessage {
            sequence: 1,
            netfn_lun: 0x18,
            command: 1,
            data: vec![],
        };
        let second = IpmiMessage {
            sequence: 2,
            netfn_lun: 0x18,
            command: 2,
            data: vec![],
        };
        let input = encode_message(&first)
            .into_iter()
            .chain(encode_message(&second))
            .collect::<Vec<_>>();
        client.write_all(&input).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(
            response,
            encode_message(&IpmiMessage {
                sequence: 2,
                netfn_lun: 0x1c,
                command: 2,
                data: vec![0],
            })
        );
        handle.join().unwrap();
    }
}
