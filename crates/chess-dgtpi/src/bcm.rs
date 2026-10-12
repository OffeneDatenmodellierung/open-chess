//! The DGT Pi's I²C link, on the Raspberry Pi's own hardware: the BSC1
//! I²C master on GPIO 2/3 to send, and the BSC slave on GPIO 18/19 to
//! hear the clock's answers, both wired to the DGT3000's jack. Linux has
//! no driver for the BSC slave, so, as DGT's `dgtpicom` does, the registers
//! are mapped from `/dev/mem` (which needs root) and driven directly.
//!
//! The only `unsafe` code in open-chess's device runtime is here: mapping
//! the three register pages and reading and writing them.

#![allow(unsafe_code)]

use std::io;
use std::ptr::NonNull;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fd::OwnedFd;
use rustix::fs::{Mode, OFlags, open};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

use crate::driver::{Bus, ReceiveError, SendError};
use crate::protocol::{BROADCAST, PI};

const PAGE: usize = 4096;
const GPIO: u64 = 0x20_0000;
const BSC_SLAVE: u64 = 0x21_4000;
const BSC1_MASTER: u64 = 0x80_4000;

// GPIO registers (32-bit words).
const GPFSEL0: usize = 0;
const GPFSEL1: usize = 1;
const GPLEV0: usize = 13;
// BSC slave.
const DR: usize = 0;
const RSR: usize = 1;
const SLV: usize = 2;
const CR: usize = 3;
const FR: usize = 4;
// BSC master.
const C: usize = 0;
const S: usize = 1;
const DLEN: usize = 2;
const A: usize = 3;
const FIFO: usize = 4;
const DIV: usize = 5;

/// SDA (GPIO 2) and SCL (GPIO 3), and their twins on GPIO 18 and 19.
const LINES: u32 = 0b1100 | (0b11 << 18);
/// The master's clock divider: 95 kHz, which DGT found works better than 100.
const DIVIDER: u32 = 0x0a47;

/// One mapped page of peripheral registers.
struct Regs {
    base: NonNull<u32>,
}

// SAFETY: the mapping is plain memory-mapped I/O owned by this value; it
// can be used from any one thread at a time.
unsafe impl Send for Regs {}

impl Regs {
    fn map(fd: &OwnedFd, address: u64) -> io::Result<Regs> {
        // SAFETY: a fresh shared mapping of one page of /dev/mem, at a
        // page-aligned physical address; nothing else in this process
        // refers to it, and it's unmapped on drop.
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                PAGE,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                fd,
                address,
            )
        }?;
        NonNull::new(p.cast::<u32>())
            .map(|base| Regs { base })
            .ok_or_else(|| io::Error::other("mmap returned null"))
    }

    fn read(&self, word: usize) -> u32 {
        assert!(word < PAGE / 4);
        // SAFETY: `word` is inside the mapped page; registers are read
        // with volatile loads.
        unsafe { self.base.as_ptr().add(word).read_volatile() }
    }

    fn write(&self, word: usize, value: u32) {
        assert!(word < PAGE / 4);
        // SAFETY: as for `read`.
        unsafe { self.base.as_ptr().add(word).write_volatile(value) }
    }

    fn update(&self, word: usize, f: impl FnOnce(u32) -> u32) {
        self.write(word, f(self.read(word)));
    }
}

impl Drop for Regs {
    fn drop(&mut self) {
        // SAFETY: the page mapped in `map`, unmapped once.
        let _ = unsafe { munmap(self.base.as_ptr().cast(), PAGE) };
    }
}

/// Where the SoC's peripherals are in physical memory: from the device
/// tree (as Raspberry Pi's `bcm_host` finds it), else the Pi 2/3 address.
fn peripheral_base() -> u64 {
    let Ok(ranges) = std::fs::read("/proc/device-tree/soc/ranges") else { return 0x3f00_0000 };
    let word = |i: usize| ranges.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    match (word(4), word(8)) {
        // Pi 4: a 64-bit parent address, its upper half 0.
        (Some(0), Some(b)) | (Some(b), _) => u64::from(b),
        _ => 0x3f00_0000,
    }
}

/// Why the DGT Pi's clock link couldn't be opened.
#[derive(Debug)]
pub enum OpenError {
    /// `/dev/mem` couldn't be opened or mapped (run as root).
    Memory(io::Error),
    /// A pin is in use as an output: probably not a DGT Pi.
    PinInUse(u8),
    /// The lines aren't pulled high: no clock wired, or a short.
    LinesLow,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Memory(e) => write!(f, "mapping the I²C hardware from /dev/mem (run as root): {e}"),
            OpenError::PinInUse(g) => {
                write!(f, "GPIO {g} is set as an output: in use, so probably not a DGT Pi")
            }
            OpenError::LinesLow => f.write_str("the clock's I²C lines are low: no DGT3000 wired, or a short"),
        }
    }
}

impl std::error::Error for OpenError {}

/// The DGT Pi's link to its clock.
pub struct BcmBus {
    gpio: Regs,
    slave: Regs,
    master: Regs,
}

impl std::fmt::Debug for BcmBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BcmBus")
    }
}

impl BcmBus {
    /// Map the hardware, check the wiring and set up both I²C blocks.
    pub fn open() -> Result<BcmBus, OpenError> {
        let base = peripheral_base();
        let fd = open("/dev/mem", OFlags::RDWR | OFlags::SYNC | OFlags::CLOEXEC, Mode::empty())
            .map_err(|e| OpenError::Memory(e.into()))?;
        let map = |offset| Regs::map(&fd, base + offset).map_err(OpenError::Memory);
        let bus = BcmBus { gpio: map(GPIO)?, slave: map(BSC_SLAVE)?, master: map(BSC1_MASTER)? };

        // A pin set as an output is in use for something else.
        let (f0, f1) = (bus.gpio.read(GPFSEL0), bus.gpio.read(GPFSEL1));
        for (gpio, fsel, shift) in [(2, f0, 6), (3, f0, 9), (18, f1, 24), (19, f1, 27)] {
            if (fsel >> shift) & 0b111 == 0b001 {
                return Err(OpenError::PinInUse(gpio));
            }
        }
        bus.inputs();
        thread::sleep(Duration::from_micros(1));
        // All four lines should be pulled high.
        if bus.gpio.read(GPLEV0) & LINES != LINES {
            return Err(OpenError::LinesLow);
        }
        let mut bus = bus;
        bus.reset();
        Ok(bus)
    }

    fn inputs(&self) {
        self.gpio.update(GPFSEL0, |v| v & 0xffff_f03f);
        self.gpio.update(GPFSEL1, |v| v & 0xc0ff_ffff);
    }

    fn lines_low(&self) -> bool {
        self.gpio.read(GPLEV0) & 0b1100 != 0b1100
    }

    /// The slave is receiving, or has bytes waiting.
    fn slave_busy(&self) -> bool {
        let fr = self.slave.read(FR);
        fr & 0x20 != 0 || fr & 0x02 == 0
    }
}

impl Bus for BcmBus {
    fn send(&mut self, message: &[u8], to: u8, listen: u8) -> Result<(), SendError> {
        let len = message.get(2).map_or(0, |&l| usize::from(l)).min(message.len());
        if len < 2 {
            return Err(SendError::Silent);
        }
        let m = &self.master;
        m.write(A, u32::from(to));
        m.write(DLEN, (len - 1) as u32);
        m.write(C, 0x10); // clear the FIFO
        let mut n = 1;
        while n < len && m.read(S) & 0x10 != 0 {
            m.write(FIFO, u32::from(message[n]));
            n += 1;
        }

        // The bus is free once the lines have stayed high and the slave
        // idle for 256 looks in a row (the clock sends queued messages
        // 50 µs after the last one).
        let until = Instant::now() + Duration::from_millis(10);
        let mut quiet = 0;
        while quiet < 256 {
            quiet = if self.lines_low() || self.slave_busy() { 0 } else { quiet + 1 };
            if Instant::now() > until {
                return Err(SendError::Timeout);
            }
        }

        self.listen(listen);
        let m = &self.master;
        m.write(S, 0x302); // clear the flags
        m.write(C, 0x8080); // enable and start

        let until = Instant::now() + Duration::from_millis(10);
        while n < len {
            while m.read(S) & 0x10 == 0 {
                if m.read(S) & 0x02 != 0 {
                    break;
                }
                if Instant::now() > until {
                    self.slave.write(SLV, u32::from(BROADCAST));
                    return Err(SendError::Timeout);
                }
            }
            if m.read(S) & 0x02 != 0 {
                break;
            }
            m.write(FIFO, u32::from(message[n]));
            n += 1;
        }
        while m.read(S) & 0x02 == 0 {
            if Instant::now() > until {
                self.slave.write(SLV, u32::from(BROADCAST));
                return Err(SendError::Timeout);
            }
        }
        let status = m.read(S);
        if status & 0x300 == 0 {
            return Ok(());
        }
        self.slave.write(SLV, u32::from(BROADCAST));
        if status & 0x100 != 0 {
            m.write(S, 0x100); // not acknowledged
        }
        if status & 0x200 != 0 {
            m.write(S, 0x200); // clock-stretch timeout
            return Err(SendError::Collision);
        }
        m.update(C, |v| v | 0x10);
        if self.lines_low() || self.slave_busy() {
            return Err(SendError::Collision);
        }
        Err(SendError::Silent)
    }

    fn listen(&mut self, address: u8) {
        self.slave.write(SLV, u32::from(address));
    }

    fn receive(&mut self) -> Option<Result<Vec<u8>, ReceiveError>> {
        if !self.slave_busy() {
            return None;
        }
        let mut m = vec![(self.slave.read(SLV) as u8) << 1];
        let until = Instant::now() + Duration::from_millis(10);
        while self.slave_busy() {
            if Instant::now() > until {
                return Some(Err(ReceiveError::Timeout));
            }
            if self.slave.read(FR) & 0x02 == 0 {
                m.push(self.slave.read(DR) as u8);
                if m.len() > 2 && m.len() >= usize::from(m[2]) {
                    break;
                }
                if m.len() >= 256 {
                    return Some(Err(ReceiveError::Overrun));
                }
            } else {
                // The next byte takes about 70 µs.
                thread::sleep(Duration::from_micros(10));
            }
        }
        self.slave.write(SLV, u32::from(BROADCAST));
        if self.slave.read(RSR) & 1 != 0 {
            self.slave.write(RSR, 0);
            return Some(Err(ReceiveError::Overrun));
        }
        (m.len() > 1).then_some(Ok(m))
    }

    fn reset(&mut self) {
        let (g, s, m) = (&self.gpio, &self.slave, &self.master);
        s.write(CR, 0);
        m.write(C, 0x10);
        m.write(C, 0);
        // Through inputs, to unstick a hung master.
        g.update(GPFSEL0, |v| v & 0xffff_f03f);
        g.update(GPFSEL1, |v| v & 0x00ff_ffff);
        m.write(DLEN, 0);
        while s.read(FR) & 0x02 == 0 {
            let _ = s.read(DR);
        }
        thread::sleep(Duration::from_millis(2));
        s.write(CR, 0);
        m.write(S, 0x302);
        m.write(C, 10);
        // GPIO 2/3 to the master (ALT0), 18/19 to the slave (ALT3).
        g.update(GPFSEL0, |v| v | 0x900);
        g.update(GPFSEL1, |v| v | 0x3f00_0000);
        thread::sleep(Duration::from_millis(1));
        s.write(CR, 0x80); // break, off
        s.write(CR, 0x205); // receive, I²C, on
        s.write(SLV, u32::from(BROADCAST));
        s.write(RSR, 0);
        m.write(DIV, DIVIDER);
    }
}

impl Drop for BcmBus {
    fn drop(&mut self) {
        // Stop hearing broadcasts, switch the slave off, pins back to inputs.
        self.slave.write(SLV, u32::from(PI));
        self.slave.write(CR, 0);
        self.inputs();
    }
}
