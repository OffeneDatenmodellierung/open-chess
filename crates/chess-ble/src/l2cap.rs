//! The attribute protocol's L2CAP channel to a Bluetooth LE device, through
//! the Linux kernel's Bluetooth sockets (as BlueZ's own tools use them).
//!
//! The board is found by its address (`bluetoothctl` → `scan on` shows it).
//! The only `unsafe` in this crate is here: telling the kernel the
//! Bluetooth socket address, which has no safe wrapper.

#![allow(unsafe_code)]

use std::io;
use std::str::FromStr;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::{AddressFamily, SocketType, bind, connect, socket};

use crate::gatt::Link;

/// The fixed L2CAP channel for the attribute protocol.
const ATT_CID: u16 = 4;

/// A Bluetooth device address, as written (`AA:BB:CC:DD:EE:FF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address(pub [u8; 6]);

impl FromStr for Address {
    type Err = String;

    fn from_str(s: &str) -> Result<Address, String> {
        let parts: Vec<_> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(format!("{s}: a Bluetooth address is AA:BB:CC:DD:EE:FF"));
        }
        let mut a = [0u8; 6];
        for (slot, p) in a.iter_mut().zip(parts) {
            *slot = u8::from_str_radix(p, 16)
                .map_err(|_| format!("{s}: a Bluetooth address is AA:BB:CC:DD:EE:FF"))?;
        }
        Ok(Address(a))
    }
}

/// Whether the device's address is its public one or a random (static)
/// one; boards use either. `bluetoothctl info` says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    Public,
    Random,
}

/// `struct sockaddr_l2` from `<bluetooth/l2cap.h>`.
#[repr(C)]
struct SockaddrL2 {
    family: u16,
    psm: u16,
    /// Little-endian: the address's bytes reversed.
    bdaddr: [u8; 6],
    cid: u16,
    bdaddr_type: u8,
}

struct L2Addr {
    address: [u8; 6],
    kind: u8,
}

const BDADDR_LE_PUBLIC: u8 = 1;
const BDADDR_LE_RANDOM: u8 = 2;

// SAFETY: `with_sockaddr` passes a pointer to a properly initialised
// `sockaddr_l2` on its stack and that struct's size, valid for the call.
unsafe impl SocketAddrArg for L2Addr {
    unsafe fn with_sockaddr<R>(&self, f: impl FnOnce(*const SocketAddrOpaque, SocketAddrLen) -> R) -> R {
        let mut bdaddr = self.address;
        bdaddr.reverse();
        let sa = SockaddrL2 {
            family: AddressFamily::BLUETOOTH.as_raw(),
            psm: 0,
            bdaddr,
            cid: ATT_CID.to_le(),
            bdaddr_type: self.kind,
        };
        f(std::ptr::from_ref(&sa).cast(), size_of::<SockaddrL2>() as SocketAddrLen)
    }
}

/// An ATT channel to a device.
#[derive(Debug)]
pub struct Channel {
    fd: OwnedFd,
}

impl Channel {
    /// Connect to a device (this takes a few seconds, or fails if the
    /// device is off, out of range, or connected to something else).
    pub fn connect(address: Address, kind: AddressType) -> io::Result<Channel> {
        let fd = socket(AddressFamily::BLUETOOTH, SocketType::SEQPACKET, None).map_err(|e| {
            if e == rustix::io::Errno::AFNOSUPPORT {
                io::Error::other(
                    "this computer has no Bluetooth (or the kernel's Bluetooth support isn't loaded)",
                )
            } else {
                e.into()
            }
        })?;
        // Our side: any adapter, LE public, the ATT channel.
        bind(&fd, &L2Addr { address: [0; 6], kind: BDADDR_LE_PUBLIC })?;
        let k = match kind {
            AddressType::Public => BDADDR_LE_PUBLIC,
            AddressType::Random => BDADDR_LE_RANDOM,
        };
        connect(&fd, &L2Addr { address: address.0, kind: k })?;
        Ok(Channel { fd })
    }
}

impl Link for Channel {
    fn send(&mut self, pdu: &[u8]) -> io::Result<()> {
        let n = rustix::io::write(&self.fd, pdu)?;
        if n == pdu.len() { Ok(()) } else { Err(io::Error::other("a short write")) }
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let ts = Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        };
        let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
        if poll(&mut fds, Some(&ts))? == 0 {
            return Ok(None);
        }
        let revents = fds[0].revents();
        if revents.intersects(PollFlags::HUP | PollFlags::ERR) && !revents.contains(PollFlags::IN) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the board disconnected"));
        }
        let mut buf = [0u8; 517];
        let n = rustix::io::read(&self.fd, &mut buf)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the board disconnected"));
        }
        Ok(Some(buf[..n].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_addresses() {
        assert_eq!("AA:BB:cc:01:02:03".parse(), Ok(Address([0xaa, 0xbb, 0xcc, 1, 2, 3])));
        assert!("AA:BB:CC".parse::<Address>().is_err());
        assert_eq!(size_of::<SockaddrL2>(), 14);
    }
}
