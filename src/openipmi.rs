use crate::{
    error::{RelayError, Result},
    vm_codec::IpmiMessage,
};
use std::{
    fs::OpenOptions,
    os::fd::AsRawFd,
    path::Path,
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, Instant},
};

const IPMI_SYSTEM_INTERFACE_ADDR_TYPE: i32 = 0x0c;
const IPMI_BMC_CHANNEL: i16 = 0x0f;
const IPMI_RESPONSE_RECV_TYPE: i32 = 1;
const IPMI_MAX_ADDR_SIZE: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
struct SystemInterfaceAddr {
    addr_type: i32,
    channel: i16,
    lun: u8,
}

#[repr(C)]
struct IpmiAddr {
    addr_type: i32,
    channel: i16,
    data: [u8; IPMI_MAX_ADDR_SIZE],
}

#[repr(C)]
struct IpmiMsg {
    netfn: u8,
    cmd: u8,
    data_len: u16,
    data: *mut u8,
}

#[repr(C)]
struct IpmiReq {
    addr: *mut u8,
    addr_len: u32,
    msgid: libc::c_long,
    msg: IpmiMsg,
}

#[repr(C)]
struct IpmiRecv {
    recv_type: i32,
    addr: *mut u8,
    addr_len: u32,
    msgid: libc::c_long,
    msg: IpmiMsg,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'i' as u32) << 8) | nr) as libc::c_ulong
}
const IPMICTL_SEND_COMMAND: libc::c_ulong = ioc(2, 13, std::mem::size_of::<IpmiReq>());
const IPMICTL_RECEIVE_MSG_TRUNC: libc::c_ulong = ioc(3, 11, std::mem::size_of::<IpmiRecv>());

pub struct OpenIpmi {
    file: std::fs::File,
    next_msgid: AtomicI64,
}

pub trait Backend {
    fn transact(&self, request: &IpmiMessage, timeout: Duration) -> Result<IpmiMessage>;
}

impl OpenIpmi {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            file: OpenOptions::new().read(true).write(true).open(path)?,
            next_msgid: AtomicI64::new(1),
        })
    }

    fn ioctl<T>(&self, request: libc::c_ulong, arg: *mut T) -> Result<()> {
        // SAFETY: Every caller passes a pointer to the C-compatible structure
        // selected by the matching Linux IPMI ioctl request number.
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request, arg) };
        if result < 0 {
            return Err(RelayError::OpenIpmi(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(())
    }
}

impl Backend for OpenIpmi {
    fn transact(&self, request: &IpmiMessage, timeout: Duration) -> Result<IpmiMessage> {
        if request.data.len() > u16::MAX as usize {
            return Err(RelayError::OpenIpmi("request payload is too large".into()));
        }
        let mut request_data = request.data.clone();
        let mut address = SystemInterfaceAddr {
            addr_type: IPMI_SYSTEM_INTERFACE_ADDR_TYPE,
            channel: IPMI_BMC_CHANNEL,
            lun: request.netfn_lun & 3,
        };
        // QEMU's sequence is only one byte and is quickly reused. A separate
        // monotonic msgid prevents a delayed kernel response from matching a
        // later request with the same QEMU sequence.
        let msgid = self.next_msgid.fetch_add(1, Ordering::Relaxed);
        let mut req = IpmiReq {
            addr: (&mut address as *mut SystemInterfaceAddr).cast(),
            addr_len: std::mem::size_of::<SystemInterfaceAddr>() as u32,
            msgid,
            msg: IpmiMsg {
                netfn: request.netfn_lun >> 2,
                cmd: request.command,
                data_len: request_data.len() as u16,
                data: request_data.as_mut_ptr(),
            },
        };
        self.ioctl(IPMICTL_SEND_COMMAND, &mut req as *mut IpmiReq)?;
        let mut response_data = vec![0u8; 1024];
        let mut response_addr = IpmiAddr {
            addr_type: 0,
            channel: 0,
            data: [0; IPMI_MAX_ADDR_SIZE],
        };
        let mut response = IpmiRecv {
            recv_type: 0,
            addr: (&mut response_addr as *mut IpmiAddr).cast(),
            addr_len: std::mem::size_of::<IpmiAddr>() as u32,
            msgid: 0,
            msg: IpmiMsg {
                netfn: 0,
                cmd: 0,
                data_len: response_data.len() as u16,
                data: response_data.as_mut_ptr(),
            },
        };
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RelayError::OpenIpmi("request timed out".into()));
            }
            let mut pollfd = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
            let result = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if result == 0 {
                return Err(RelayError::OpenIpmi("request timed out".into()));
            }
            response.addr_len = std::mem::size_of::<IpmiAddr>() as u32;
            response.msg.data_len = response_data.len() as u16;
            self.ioctl(IPMICTL_RECEIVE_MSG_TRUNC, &mut response as *mut IpmiRecv)?;
            if response.recv_type != IPMI_RESPONSE_RECV_TYPE || response.msgid != msgid {
                continue;
            }
            let len = response.msg.data_len as usize;
            response_data.truncate(len.min(response_data.len()));
            return Ok(IpmiMessage {
                sequence: request.sequence,
                netfn_lun: (response.msg.netfn << 2) | (response_addr.data[0] & 3),
                command: response.msg.cmd,
                data: response_data,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abi_sizes_are_expected_on_64_bit_linux() {
        assert_eq!(std::mem::size_of::<SystemInterfaceAddr>(), 8);
        assert_eq!(std::mem::size_of::<IpmiAddr>(), 40);
        assert_eq!(std::mem::size_of::<IpmiMsg>(), 16);
        assert_eq!(std::mem::size_of::<IpmiReq>(), 40);
        assert_eq!(std::mem::size_of::<IpmiRecv>(), 48);
        assert_eq!(IPMICTL_SEND_COMMAND, 0x8028_690d);
        assert_eq!(IPMICTL_RECEIVE_MSG_TRUNC, 0xc030_690b);
    }
}
