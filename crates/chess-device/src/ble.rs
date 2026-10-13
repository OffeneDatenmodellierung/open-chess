//! A Bluetooth LE board's link: write commands to one characteristic, read
//! notifications from another.
//!
//! On Linux it's open-chess's own GATT client over the kernel's sockets
//! (`chess-ble`: no D-Bus, cross-compiles for a Pi as it is). On macOS and
//! Windows it's btleplug, over CoreBluetooth and Windows' Bluetooth API.
//! On a Mac a board is known by the id macOS gives it, not its address.

use std::io;
use std::time::Duration;

use crate::discover::Found;

/// A connected board.
pub trait Transport: Send {
    /// Write to a characteristic (by UUID).
    fn write(&mut self, uuid: &str, data: &[u8]) -> io::Result<()>;
    /// Notifications from the subscribed characteristic, for up to `wait`.
    fn notifications(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>>;
}

/// Connect to a board by its address (its id, on a Mac) and subscribe to
/// the characteristic with UUID `notify`.
pub fn connect(address: &str, random: bool, notify: &str) -> io::Result<Box<dyn Transport>> {
    imp::connect(address, random, notify)
}

/// Bluetooth LE boards nearby.
pub fn scan(secs: u64) -> Result<Vec<Found>, String> {
    imp::scan(secs)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Found, Transport};
    use chess_ble::l2cap::{Address, AddressType, Channel};
    use chess_ble::{Gatt, Uuid};
    use std::io;
    use std::time::Duration;

    struct Kernel {
        gatt: Gatt<Channel>,
        notify: u16,
    }

    fn uuid(s: &str) -> io::Result<Uuid> {
        s.parse().map_err(io::Error::other)
    }

    pub fn connect(address: &str, random: bool, notify: &str) -> io::Result<Box<dyn Transport>> {
        let a: Address = address.parse().map_err(io::Error::other)?;
        let kind = if random { AddressType::Random } else { AddressType::Public };
        let mut gatt = Gatt::new(Channel::connect(a, kind)?);
        gatt.exchange_mtu(185)?;
        let notify = gatt.subscribe(uuid(notify)?)?;
        Ok(Box::new(Kernel { gatt, notify }))
    }

    impl Transport for Kernel {
        fn write(&mut self, to: &str, data: &[u8]) -> io::Result<()> {
            self.gatt.write_to(uuid(to)?, data)
        }

        fn notifications(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            let mut wait = wait;
            while let Some((h, data)) = self.gatt.notification(wait)? {
                if h == self.notify {
                    out.push(data);
                }
                wait = Duration::ZERO;
            }
            Ok(out)
        }
    }

    pub fn scan(secs: u64) -> Result<Vec<Found>, String> {
        crate::discover::ble_scan(secs)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use super::{Found, Transport};
    use crate::discover::BleKind;
    use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
    use btleplug::platform::{Adapter, Manager, Peripheral};
    use futures_util::StreamExt;
    use std::io;
    use std::sync::mpsc;
    use std::time::Duration;

    fn err(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn runtime() -> io::Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()
    }

    async fn adapter() -> io::Result<Adapter> {
        let manager = Manager::new().await.map_err(err)?;
        manager
            .adapters()
            .await
            .map_err(err)?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("no Bluetooth adapter"))
    }

    async fn find(central: &Adapter, id: &str, secs: u64) -> io::Result<Peripheral> {
        central.start_scan(ScanFilter::default()).await.map_err(err)?;
        for _ in 0..(secs * 4) {
            for p in central.peripherals().await.map_err(err)? {
                if p.id().to_string().eq_ignore_ascii_case(id)
                    || p.address().to_string().eq_ignore_ascii_case(id)
                {
                    let _ = central.stop_scan().await;
                    return Ok(p);
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let _ = central.stop_scan().await;
        Err(io::Error::new(io::ErrorKind::NotFound, format!("{id}: not found nearby")))
    }

    pub fn scan(secs: u64) -> Result<Vec<Found>, String> {
        let rt = runtime().map_err(|e| e.to_string())?;
        rt.block_on(async {
            let central = adapter().await.map_err(|e| e.to_string())?;
            central.start_scan(ScanFilter::default()).await.map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let mut out = Vec::new();
            for p in central.peripherals().await.map_err(|e| e.to_string())? {
                let name =
                    p.properties().await.ok().flatten().and_then(|pr| pr.local_name).unwrap_or_default();
                if let Some(kind) = BleKind::from_name(&name) {
                    out.push(Found::Ble { address: p.id().to_string(), name, kind, random: false });
                }
            }
            let _ = central.stop_scan().await;
            Ok(out)
        })
    }

    enum Out {
        Write(String, Vec<u8>, mpsc::Sender<io::Result<()>>),
    }

    struct Btle {
        writes: tokio::sync::mpsc::UnboundedSender<Out>,
        notifications: mpsc::Receiver<Vec<u8>>,
    }

    pub fn connect(address: &str, _random: bool, notify: &str) -> io::Result<Box<dyn Transport>> {
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
        let (note_tx, notifications) = mpsc::channel::<Vec<u8>>();
        let (writes, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
        let (address, notify) = (address.to_owned(), notify.to_owned());
        std::thread::Builder::new().name("bluetooth".into()).spawn(move || {
            let rt = match runtime() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            rt.block_on(async move {
                let setup = async {
                    let central = adapter().await?;
                    let p = find(&central, &address, 15).await?;
                    p.connect().await.map_err(err)?;
                    p.discover_services().await.map_err(err)?;
                    let chars = p.characteristics();
                    let n = chars
                        .iter()
                        .find(|c| c.uuid.to_string().eq_ignore_ascii_case(&notify))
                        .cloned()
                        .ok_or_else(|| io::Error::other(format!("the board has no characteristic {notify}")))?;
                    p.subscribe(&n).await.map_err(err)?;
                    let stream = p.notifications().await.map_err(err)?;
                    Ok::<_, io::Error>((p, chars, stream))
                };
                let (p, chars, mut stream) = match setup.await {
                    Ok(x) => {
                        let _ = ready_tx.send(Ok(()));
                        x
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                loop {
                    tokio::select! {
                        n = stream.next() => {
                            let Some(n) = n else { return };
                            if note_tx.send(n.value).is_err() {
                                return;
                            }
                        }
                        w = write_rx.recv() => {
                            let Some(Out::Write(uuid, data, reply)) = w else { return };
                            let r = match chars.iter().find(|c| c.uuid.to_string().eq_ignore_ascii_case(&uuid)) {
                                Some(c) => {
                                    let kind = if c.properties.contains(btleplug::api::CharPropFlags::WRITE) {
                                        WriteType::WithResponse
                                    } else {
                                        WriteType::WithoutResponse
                                    };
                                    p.write(c, &data, kind).await.map_err(err)
                                }
                                None => Err(io::Error::other(format!("the board has no characteristic {uuid}"))),
                            };
                            let _ = reply.send(r);
                        }
                    }
                }
            });
        })?;
        ready_rx.recv().map_err(|_| io::Error::other("the Bluetooth thread stopped"))??;
        Ok(Box::new(Btle { writes, notifications }))
    }

    impl Transport for Btle {
        fn write(&mut self, uuid: &str, data: &[u8]) -> io::Result<()> {
            let (tx, rx) = mpsc::channel();
            self.writes
                .send(Out::Write(uuid.to_owned(), data.to_vec(), tx))
                .map_err(|_| io::Error::other("the board disconnected"))?;
            rx.recv().map_err(|_| io::Error::other("the board disconnected"))?
        }

        fn notifications(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            match self.notifications.recv_timeout(wait) {
                Ok(n) => out.push(n),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(out),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the board disconnected"));
                }
            }
            out.extend(self.notifications.try_iter());
            Ok(out)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use super::{Found, Transport};
    use std::io;

    pub fn connect(_: &str, _: bool, _: &str) -> io::Result<Box<dyn Transport>> {
        Err(io::Error::other("Bluetooth LE boards need Linux, macOS or Windows"))
    }

    pub fn scan(_: u64) -> Result<Vec<Found>, String> {
        Err("Bluetooth LE boards need Linux, macOS or Windows".into())
    }
}
