//! Just enough GATT, over the attribute protocol (ATT), for a chess board:
//! find characteristics by UUID, subscribe to one, write to another, and
//! take notifications. No I/O: it runs over any [`Link`] that carries ATT
//! packets (the L2CAP socket, or a simulated board).

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::uuid::Uuid;

/// Carries ATT packets to and from the board.
pub trait Link {
    fn send(&mut self, pdu: &[u8]) -> io::Result<()>;
    /// The next packet, or `None` after `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

/// ATT opcodes.
pub mod op {
    pub const ERROR: u8 = 0x01;
    pub const MTU_REQ: u8 = 0x02;
    pub const MTU_RSP: u8 = 0x03;
    pub const FIND_INFO_REQ: u8 = 0x04;
    pub const FIND_INFO_RSP: u8 = 0x05;
    pub const READ_BY_TYPE_REQ: u8 = 0x08;
    pub const READ_BY_TYPE_RSP: u8 = 0x09;
    pub const WRITE_REQ: u8 = 0x12;
    pub const WRITE_RSP: u8 = 0x13;
    pub const NOTIFY: u8 = 0x1b;
    pub const INDICATE: u8 = 0x1d;
    pub const CONFIRM: u8 = 0x1e;
    pub const WRITE_CMD: u8 = 0x52;
}

/// "Attribute not found": the end of a search.
const NOT_FOUND: u8 = 0x0a;
/// The characteristic declaration's type.
const CHARACTERISTIC: u16 = 0x2803;
/// The client characteristic configuration descriptor (notifications on/off).
const CCCD: u16 = 0x2902;
/// How long a request may take (the ATT spec's transaction timeout).
const TIMEOUT: Duration = Duration::from_secs(30);

/// A characteristic: its declaration, properties, value handle and UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Characteristic {
    pub declaration: u16,
    pub properties: u8,
    pub value: u16,
    pub uuid: Uuid,
}

impl Characteristic {
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;
}

fn error(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// A GATT client on a link.
#[derive(Debug)]
pub struct Gatt<L: Link> {
    link: L,
    mtu: u16,
    notifications: VecDeque<(u16, Vec<u8>)>,
    characteristics: Vec<Characteristic>,
}

impl<L: Link> Gatt<L> {
    pub fn new(link: L) -> Gatt<L> {
        Gatt { link, mtu: 23, notifications: VecDeque::new(), characteristics: Vec::new() }
    }

    pub fn link(&self) -> &L {
        &self.link
    }

    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// Send a request and wait for its response (or an error response),
    /// keeping any notifications that arrive meanwhile.
    fn request(&mut self, pdu: &[u8], response: u8) -> io::Result<Result<Vec<u8>, u8>> {
        self.link.send(pdu)?;
        let until = Instant::now() + TIMEOUT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the board didn't answer"));
            }
            let Some(p) = self.link.recv(left)? else { continue };
            match p.first().copied() {
                Some(o) if o == response => return Ok(Ok(p)),
                Some(op::ERROR) if p.len() >= 5 && p[1] == pdu[0] => return Ok(Err(p[4])),
                _ => self.take(&p)?,
            }
        }
    }

    /// A packet that isn't a response: keep notifications, confirm
    /// indications.
    fn take(&mut self, p: &[u8]) -> io::Result<()> {
        match p.first().copied() {
            Some(op::NOTIFY) if p.len() >= 3 => {
                self.notifications.push_back((u16::from_le_bytes([p[1], p[2]]), p[3..].to_vec()));
            }
            Some(op::INDICATE) if p.len() >= 3 => {
                self.notifications.push_back((u16::from_le_bytes([p[1], p[2]]), p[3..].to_vec()));
                self.link.send(&[op::CONFIRM])?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Agree a larger packet size (what the board allows, up to `mtu`).
    pub fn exchange_mtu(&mut self, mtu: u16) -> io::Result<u16> {
        let mut req = vec![op::MTU_REQ];
        req.extend_from_slice(&mtu.to_le_bytes());
        if let Ok(p) = self.request(&req, op::MTU_RSP)?
            && p.len() >= 3
        {
            self.mtu = mtu.min(u16::from_le_bytes([p[1], p[2]])).max(23);
        }
        Ok(self.mtu)
    }

    /// Every characteristic the board has (read once, then remembered).
    pub fn characteristics(&mut self) -> io::Result<Vec<Characteristic>> {
        if !self.characteristics.is_empty() {
            return Ok(self.characteristics.clone());
        }
        let mut found = Vec::new();
        let mut start = 1u16;
        loop {
            let mut req = vec![op::READ_BY_TYPE_REQ];
            req.extend_from_slice(&start.to_le_bytes());
            req.extend_from_slice(&0xffffu16.to_le_bytes());
            req.extend_from_slice(&CHARACTERISTIC.to_le_bytes());
            let p = match self.request(&req, op::READ_BY_TYPE_RSP)? {
                Ok(p) => p,
                Err(NOT_FOUND) => break,
                Err(code) => return Err(error(format!("reading characteristics: ATT error {code:#04x}"))),
            };
            let size = usize::from(*p.get(1).ok_or_else(|| error("short answer"))?);
            if size < 7 {
                return Err(error("a malformed characteristic list"));
            }
            let mut last = start;
            for entry in p[2..].chunks_exact(size) {
                let declaration = u16::from_le_bytes([entry[0], entry[1]]);
                let value = u16::from_le_bytes([entry[3], entry[4]]);
                if let Some(uuid) = Uuid::from_wire(&entry[5..]) {
                    found.push(Characteristic { declaration, properties: entry[2], value, uuid });
                }
                last = declaration;
            }
            if last == 0xffff {
                break;
            }
            start = last + 1;
        }
        self.characteristics.clone_from(&found);
        Ok(found)
    }

    /// The characteristic with this UUID.
    pub fn characteristic(&mut self, uuid: Uuid) -> io::Result<Characteristic> {
        self.characteristics()?
            .into_iter()
            .find(|c| c.uuid == uuid)
            .ok_or_else(|| error(format!("the board has no characteristic {uuid}")))
    }

    /// The configuration descriptor after a characteristic's value.
    fn cccd(&mut self, c: Characteristic) -> io::Result<u16> {
        let end = self
            .characteristics()?
            .iter()
            .map(|x| x.declaration)
            .filter(|&d| d > c.value)
            .min()
            .map_or(0xffff, |d| d - 1);
        let mut start = c.value + 1;
        while start <= end {
            let mut req = vec![op::FIND_INFO_REQ];
            req.extend_from_slice(&start.to_le_bytes());
            req.extend_from_slice(&end.to_le_bytes());
            let Ok(p) = self.request(&req, op::FIND_INFO_RSP)? else { break };
            let size = match p.get(1) {
                Some(1) => 4,
                Some(2) => 18,
                _ => break,
            };
            let mut last = start;
            for e in p[2..].chunks_exact(size) {
                let handle = u16::from_le_bytes([e[0], e[1]]);
                if Uuid::from_wire(&e[2..]) == Some(Uuid::short(CCCD)) {
                    return Ok(handle);
                }
                last = handle;
            }
            if last == 0xffff {
                break;
            }
            start = last + 1;
        }
        // Usually right after the value.
        Ok(c.value + 1)
    }

    /// Turn on notifications (or indications) from a characteristic.
    /// Returns its value handle, which notifications carry.
    pub fn subscribe(&mut self, uuid: Uuid) -> io::Result<u16> {
        let c = self.characteristic(uuid)?;
        let handle = self.cccd(c)?;
        let bits: u16 = if c.properties & Characteristic::NOTIFY != 0 { 1 } else { 2 };
        self.write(handle, &bits.to_le_bytes(), true)?;
        Ok(c.value)
    }

    /// Write to a handle, waiting for the board's answer or not.
    pub fn write(&mut self, handle: u16, data: &[u8], with_response: bool) -> io::Result<()> {
        let max = usize::from(self.mtu) - 3;
        for part in data.chunks(max.max(1)) {
            let mut req = vec![if with_response { op::WRITE_REQ } else { op::WRITE_CMD }];
            req.extend_from_slice(&handle.to_le_bytes());
            req.extend_from_slice(part);
            if with_response {
                if let Err(code) = self.request(&req, op::WRITE_RSP)? {
                    return Err(error(format!("writing to {handle:#06x}: ATT error {code:#04x}")));
                }
            } else {
                self.link.send(&req)?;
            }
        }
        Ok(())
    }

    /// Write to the characteristic with this UUID, the way it accepts.
    pub fn write_to(&mut self, uuid: Uuid, data: &[u8]) -> io::Result<()> {
        let c = self.characteristic(uuid)?;
        let with_response = c.properties & Characteristic::WRITE_WITHOUT_RESPONSE == 0;
        self.write(c.value, data, with_response)
    }

    /// The next notification (handle and value), waiting up to `timeout`.
    pub fn notification(&mut self, timeout: Duration) -> io::Result<Option<(u16, Vec<u8>)>> {
        if let Some(n) = self.notifications.pop_front() {
            return Ok(Some(n));
        }
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let Some(p) = self.link.recv(left)? else { return Ok(None) };
            self.take(&p)?;
            if let Some(n) = self.notifications.pop_front() {
                return Ok(Some(n));
            }
            if Instant::now() >= until {
                return Ok(None);
            }
        }
    }
}

/// A simulated GATT server, for tests: characteristics with values and
/// notifications to send.
pub mod sim {
    use super::{CCCD, CHARACTERISTIC, Characteristic, Link, NOT_FOUND, op};
    use crate::uuid::Uuid;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct State {
        /// (handle, type, value) — the attribute table.
        attributes: Vec<(u16, Uuid, Vec<u8>)>,
        outbox: VecDeque<Vec<u8>>,
        /// (handle, value) written by the client.
        written: Vec<(u16, Vec<u8>)>,
    }

    /// The server; clones share it.
    #[derive(Debug, Clone, Default)]
    pub struct Server(Arc<Mutex<State>>);

    impl Server {
        /// A server with one service's characteristics (each with a
        /// configuration descriptor when it notifies). Returns it and
        /// each characteristic's value handle.
        pub fn new(characteristics: &[(Uuid, u8)]) -> (Server, Vec<u16>) {
            let mut attributes = vec![(1, Uuid::short(0x2800), vec![0x0d, 0x18])];
            let mut handle = 2u16;
            let mut values = Vec::new();
            for &(uuid, properties) in characteristics {
                let value = handle + 1;
                let mut decl = vec![properties];
                decl.extend_from_slice(&value.to_le_bytes());
                decl.extend(uuid.to_wire());
                attributes.push((handle, Uuid::short(CHARACTERISTIC), decl));
                attributes.push((value, uuid, Vec::new()));
                values.push(value);
                handle += 2;
                if properties & (Characteristic::NOTIFY | Characteristic::INDICATE) != 0 {
                    attributes.push((handle, Uuid::short(CCCD), vec![0, 0]));
                    handle += 1;
                }
            }
            (Server(Arc::new(Mutex::new(State { attributes, ..State::default() }))), values)
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        /// Notify a value, if the client subscribed to it.
        pub fn notify(&self, value_handle: u16, data: &[u8]) -> bool {
            let mut s = self.state();
            let on = s.attributes.iter().any(|(h, t, v)| {
                *h > value_handle
                    && *h <= value_handle + 1
                    && *t == Uuid::short(CCCD)
                    && v.first() == Some(&1)
            });
            if on {
                let mut p = vec![op::NOTIFY];
                p.extend_from_slice(&value_handle.to_le_bytes());
                p.extend_from_slice(data);
                s.outbox.push_back(p);
            }
            on
        }

        /// What the client wrote, in order.
        pub fn written(&self) -> Vec<(u16, Vec<u8>)> {
            self.state().written.clone()
        }
    }

    fn err(request: u8, handle: u16, code: u8) -> Vec<u8> {
        let h = handle.to_le_bytes();
        vec![op::ERROR, request, h[0], h[1], code]
    }

    impl Link for Server {
        fn send(&mut self, pdu: &[u8]) -> io::Result<()> {
            let mut s = self.state();
            let u16_at = |i: usize| u16::from_le_bytes([pdu[i], pdu[i + 1]]);
            let reply = match pdu[0] {
                op::MTU_REQ => Some(vec![op::MTU_RSP, 100, 0]),
                op::READ_BY_TYPE_REQ => {
                    let (start, end) = (u16_at(1), u16_at(3));
                    let wanted = Uuid::from_wire(&pdu[5..]);
                    let hits: Vec<_> = s
                        .attributes
                        .iter()
                        .filter(|(h, t, _)| *h >= start && *h <= end && Some(*t) == wanted)
                        .collect();
                    match hits.first() {
                        None => Some(err(pdu[0], start, NOT_FOUND)),
                        Some(first) => {
                            // One entry size per response, as ATT requires.
                            let size = first.2.len() + 2;
                            let mut p = vec![op::READ_BY_TYPE_RSP, size as u8];
                            for (h, _, v) in hits.iter().take_while(|x| x.2.len() + 2 == size).take(2) {
                                p.extend_from_slice(&h.to_le_bytes());
                                p.extend_from_slice(v);
                            }
                            Some(p)
                        }
                    }
                }
                op::FIND_INFO_REQ => {
                    let (start, end) = (u16_at(1), u16_at(3));
                    match s
                        .attributes
                        .iter()
                        .find(|(h, t, _)| *h >= start && *h <= end && t.to_wire().len() == 2)
                    {
                        None => Some(err(pdu[0], start, NOT_FOUND)),
                        Some((h, t, _)) => {
                            let mut p = vec![op::FIND_INFO_RSP, 1];
                            p.extend_from_slice(&h.to_le_bytes());
                            p.extend(t.to_wire());
                            Some(p)
                        }
                    }
                }
                op::WRITE_REQ | op::WRITE_CMD => {
                    let handle = u16_at(1);
                    let data = pdu[3..].to_vec();
                    s.written.push((handle, data.clone()));
                    if let Some(a) = s.attributes.iter_mut().find(|(h, _, _)| *h == handle) {
                        a.2 = data;
                    }
                    (pdu[0] == op::WRITE_REQ).then(|| vec![op::WRITE_RSP])
                }
                _ => None,
            };
            if let Some(r) = reply {
                s.outbox.push_back(r);
            }
            Ok(())
        }

        fn recv(&mut self, _timeout: Duration) -> io::Result<Option<Vec<u8>>> {
            Ok(self.state().outbox.pop_front())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sim::Server;
    use super::*;

    fn board() -> (Server, Vec<u16>, Uuid, Uuid) {
        let rx: Uuid = "1b7e8262-2877-41c3-b46e-cf057c562023".parse().unwrap();
        let tx: Uuid = "1b7e8272-2877-41c3-b46e-cf057c562023".parse().unwrap();
        let (server, handles) = Server::new(&[
            (Uuid::short(0x2a00), 0x02),
            (rx, Characteristic::NOTIFY),
            (tx, Characteristic::WRITE),
        ]);
        (server, handles, rx, tx)
    }

    #[test]
    fn finds_characteristics_subscribes_and_writes() {
        let (server, handles, rx, tx) = board();
        let mut g = Gatt::new(server.clone());
        assert_eq!(g.exchange_mtu(185).unwrap(), 100);
        let chars = g.characteristics().unwrap();
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[1].uuid, rx);
        assert_eq!(chars[1].value, handles[1]);

        let value = g.subscribe(rx).unwrap();
        assert_eq!(value, handles[1]);
        assert_eq!(server.written(), [(handles[1] + 1, vec![1, 0])]);

        g.write_to(tx, &[0x21, 0x01, 0x00]).unwrap();
        assert_eq!(server.written()[1], (handles[2], vec![0x21, 0x01, 0x00]));

        assert!(server.notify(value, &[0x01, 0x24, 0xaa]));
        assert_eq!(g.notification(Duration::ZERO).unwrap(), Some((value, vec![0x01, 0x24, 0xaa])));
        assert_eq!(g.notification(Duration::ZERO).unwrap(), None);
    }

    #[test]
    fn a_missing_characteristic_is_an_error() {
        let (server, _, _, _) = board();
        let mut g = Gatt::new(server);
        assert!(g.characteristic(Uuid::short(0x1234)).is_err());
    }
}
