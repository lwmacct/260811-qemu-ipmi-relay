use crate::{
    error::{RelayError, Result},
    openipmi::Backend,
    vm_codec::{Decoder, Frame, IpmiMessage, encode_message},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc::{SyncSender, sync_channel},
    thread::{self, JoinHandle},
    time::Duration,
};
use tracing::{debug, warn};

struct BmcJob {
    request: IpmiMessage,
    response: SyncSender<Result<IpmiMessage>>,
}

#[derive(Clone)]
pub struct BmcDispatcher {
    requests: SyncSender<BmcJob>,
}

impl BmcDispatcher {
    fn transact(&self, request: IpmiMessage) -> Result<IpmiMessage> {
        let (response_tx, response_rx) = sync_channel(1);
        self.requests
            .send(BmcJob {
                request,
                response: response_tx,
            })
            .map_err(|_| RelayError::BmcWorkerUnavailable)?;
        response_rx
            .recv()
            .map_err(|_| RelayError::BmcWorkerUnavailable)?
    }
}

pub fn start_bmc_worker<B>(
    mut backend: B,
    timeout: Duration,
    queue_depth: usize,
) -> Result<(BmcDispatcher, JoinHandle<()>)>
where
    B: Backend + Send + 'static,
{
    let (request_tx, request_rx) = sync_channel::<BmcJob>(queue_depth);
    let handle = thread::Builder::new()
        .name("qemu-ipmi-bmc".into())
        .spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let result = backend.transact(&job.request, timeout);
                let _ = job.response.send(result);
            }
        })?;
    Ok((
        BmcDispatcher {
            requests: request_tx,
        },
        handle,
    ))
}

pub fn serve_connection(
    mut stream: UnixStream,
    dispatcher: &BmcDispatcher,
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
                    let sequence = request.sequence;
                    let netfn_lun = request.netfn_lun;
                    let command = request.command;
                    debug!(sequence, netfn_lun, command, "forwarding IPMI request");
                    match dispatcher.transact(request) {
                        Ok(response) => stream.write_all(&encode_message(&response))?,
                        Err(error) => {
                            // Keep the chardev connected. QEMU owns the external-BMC
                            // timeout and will return IPMI_CC_TIMEOUT to the guest.
                            warn!(
                                sequence,
                                netfn_lun,
                                command,
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
    use crate::vm_codec::encode_message;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        sync::mpsc::{Receiver, SyncSender, sync_channel},
    };

    struct EchoBackend;

    impl Backend for EchoBackend {
        fn transact(&mut self, request: &IpmiMessage, _timeout: Duration) -> Result<IpmiMessage> {
            Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: request.netfn_lun.wrapping_add(4),
                command: request.command,
                data: vec![0, request.data[0]],
            })
        }
    }

    struct FailOnceBackend {
        calls: usize,
    }

    impl Backend for FailOnceBackend {
        fn transact(&mut self, request: &IpmiMessage, _timeout: Duration) -> Result<IpmiMessage> {
            if self.calls == 0 {
                self.calls += 1;
                return Err(RelayError::OpenIpmi("test failure".into()));
            }
            Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: request.netfn_lun.wrapping_add(4),
                command: request.command,
                data: vec![0],
            })
        }
    }

    struct GateBackend {
        calls: usize,
        entered: SyncSender<()>,
        release: Receiver<()>,
    }

    impl Backend for GateBackend {
        fn transact(&mut self, request: &IpmiMessage, _timeout: Duration) -> Result<IpmiMessage> {
            if self.calls == 0 {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            self.calls += 1;
            Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: request.netfn_lun.wrapping_add(4),
                command: request.command,
                data: vec![0, request.command],
            })
        }
    }

    fn spawn_connection(stream: UnixStream, dispatcher: BmcDispatcher) -> JoinHandle<Result<()>> {
        thread::spawn(move || serve_connection(stream, &dispatcher, 303))
    }

    #[test]
    fn relays_a_complete_message_without_interpreting_the_command() {
        let (dispatcher, worker) =
            start_bmc_worker(EchoBackend, Duration::from_secs(1), 8).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let connection = spawn_connection(server, dispatcher.clone());
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
        connection.join().unwrap().unwrap();
        drop(dispatcher);
        worker.join().unwrap();
    }

    #[test]
    fn backend_failure_does_not_disconnect_qemu() {
        let (dispatcher, worker) =
            start_bmc_worker(FailOnceBackend { calls: 0 }, Duration::from_secs(1), 8).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let connection = spawn_connection(server, dispatcher.clone());
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
        connection.join().unwrap().unwrap();
        drop(dispatcher);
        worker.join().unwrap();
    }

    #[test]
    fn routes_same_sequence_to_concurrent_connections() {
        let (dispatcher, worker) =
            start_bmc_worker(EchoBackend, Duration::from_secs(1), 8).unwrap();
        let (mut client_a, server_a) = UnixStream::pair().unwrap();
        let (mut client_b, server_b) = UnixStream::pair().unwrap();
        let connection_a = spawn_connection(server_a, dispatcher.clone());
        let connection_b = spawn_connection(server_b, dispatcher.clone());
        let request_a = IpmiMessage {
            sequence: 7,
            netfn_lun: 0x18,
            command: 0x01,
            data: vec![0xa1],
        };
        let request_b = IpmiMessage {
            sequence: 7,
            netfn_lun: 0x28,
            command: 0x02,
            data: vec![0xb2],
        };
        client_a.write_all(&encode_message(&request_a)).unwrap();
        client_b.write_all(&encode_message(&request_b)).unwrap();
        client_a.shutdown(std::net::Shutdown::Write).unwrap();
        client_b.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response_a = Vec::new();
        let mut response_b = Vec::new();
        client_a.read_to_end(&mut response_a).unwrap();
        client_b.read_to_end(&mut response_b).unwrap();
        assert_eq!(
            response_a,
            encode_message(&IpmiMessage {
                sequence: 7,
                netfn_lun: 0x1c,
                command: 0x01,
                data: vec![0, 0xa1],
            })
        );
        assert_eq!(
            response_b,
            encode_message(&IpmiMessage {
                sequence: 7,
                netfn_lun: 0x2c,
                command: 0x02,
                data: vec![0, 0xb2],
            })
        );
        connection_a.join().unwrap().unwrap();
        connection_b.join().unwrap().unwrap();
        drop(dispatcher);
        worker.join().unwrap();
    }

    #[test]
    fn disconnected_client_does_not_stop_other_connections() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let (dispatcher, worker) = start_bmc_worker(
            GateBackend {
                calls: 0,
                entered: entered_tx,
                release: release_rx,
            },
            Duration::from_secs(1),
            8,
        )
        .unwrap();
        let (mut client_a, server_a) = UnixStream::pair().unwrap();
        let connection_a = spawn_connection(server_a, dispatcher.clone());
        client_a
            .write_all(&encode_message(&IpmiMessage {
                sequence: 1,
                netfn_lun: 0x18,
                command: 0x01,
                data: vec![],
            }))
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(client_a);

        let (mut client_b, server_b) = UnixStream::pair().unwrap();
        let connection_b = spawn_connection(server_b, dispatcher.clone());
        client_b
            .write_all(&encode_message(&IpmiMessage {
                sequence: 2,
                netfn_lun: 0x18,
                command: 0x02,
                data: vec![],
            }))
            .unwrap();
        client_b.shutdown(std::net::Shutdown::Write).unwrap();
        release_tx.send(()).unwrap();

        let mut response_b = Vec::new();
        client_b.read_to_end(&mut response_b).unwrap();
        assert_eq!(
            response_b,
            encode_message(&IpmiMessage {
                sequence: 2,
                netfn_lun: 0x1c,
                command: 0x02,
                data: vec![0, 0x02],
            })
        );
        assert!(connection_a.join().unwrap().is_err());
        connection_b.join().unwrap().unwrap();
        drop(dispatcher);
        worker.join().unwrap();
    }
}
