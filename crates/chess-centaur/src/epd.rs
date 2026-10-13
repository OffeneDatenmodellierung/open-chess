//! The Centaur's 2.9-inch e-paper panel (a Waveshare "2.9inch D", UC8151
//! controller): its commands and refresh tables, over any [`Panel`] that
//! can send commands and data. The command sequence is Waveshare's
//! (MIT-licensed), as DGTCentaurMods drives the Centaur's panel.

use std::time::Duration;

use crate::screen::{HEIGHT, WIDTH};

/// What the controller needs: the reset line, commands and data.
pub trait Panel {
    fn reset(&mut self);
    fn command(&mut self, c: u8);
    fn data(&mut self, d: &[u8]);
    fn wait(&mut self, d: Duration);
}

const LUT_VCOM: [u8; 44] = {
    let mut l = [0u8; 44];
    l[1] = 0x19;
    l[2] = 0x01;
    l[5] = 0x01;
    l
};

const fn lut(first: u8) -> [u8; 42] {
    let mut l = [0u8; 42];
    l[0] = first;
    l[1] = 0x19;
    l[2] = 0x01;
    l[5] = 0x01;
    l
}

/// The partial-refresh tables: white→white, black→white (0x80),
/// white→black (0x40), black→black.
const LUT_WW: [u8; 42] = lut(0x00);
const LUT_BW: [u8; 42] = lut(0x80);
const LUT_WB: [u8; 42] = lut(0x40);
const LUT_BB: [u8; 42] = lut(0x00);

const BYTES: usize = WIDTH / 8 * HEIGHT;

/// The panel.
pub struct Epd<P: Panel> {
    panel: P,
}

impl<P: Panel> Epd<P> {
    pub fn new(panel: P) -> Epd<P> {
        Epd { panel }
    }

    pub fn panel(&self) -> &P {
        &self.panel
    }

    /// Power on and set up for full refreshes.
    pub fn init(&mut self) {
        let p = &mut self.panel;
        p.reset();
        p.command(0x04); // power on
        p.wait(Duration::from_millis(100));
        p.command(0x00); // panel setting: LUT from OTP
        p.data(&[0x1f]);
        p.command(0x61); // resolution 128 × 296
        p.data(&[0x80, 0x01, 0x28]);
        p.command(0x50); // VCOM and data interval
        p.data(&[0x97]);
    }

    fn refresh(&mut self, d: Duration) {
        self.panel.command(0x12);
        self.panel.wait(d);
    }

    /// Draw a whole picture (panel bytes) with a full refresh: slow (a few
    /// seconds, with flashing) but clean.
    pub fn full(&mut self, bytes: &[u8]) {
        self.panel.command(0x10);
        self.panel.data(&[0x00; BYTES]);
        self.panel.command(0x13);
        self.panel.data(&bytes[..BYTES.min(bytes.len())]);
        self.refresh(Duration::from_secs(3));
    }

    fn partial_registers(&mut self) {
        let p = &mut self.panel;
        p.command(0x01); // power setting
        p.data(&[0x03, 0x00, 0x2b, 0x2b, 0x03]);
        p.command(0x06); // booster soft start
        p.data(&[0x17, 0x17, 0x17]);
        p.command(0x04);
        p.wait(Duration::from_millis(100));
        p.command(0x00); // panel setting: LUT from registers
        p.data(&[0xbf]);
        p.command(0x30); // PLL: 100 Hz
        p.data(&[0x3a]);
        p.command(0x61);
        p.data(&[WIDTH as u8, (HEIGHT >> 8) as u8, (HEIGHT & 0xff) as u8]);
        p.command(0x82); // VCOM DC
        p.data(&[0x12]);
        p.command(0x50);
        p.data(&[0x97]);
        p.command(0x20);
        p.data(&LUT_VCOM);
        p.command(0x21);
        p.data(&LUT_WW);
        p.command(0x22);
        p.data(&LUT_BW);
        p.command(0x23);
        p.data(&LUT_WB);
        p.command(0x24);
        p.data(&LUT_BB);
    }

    /// Draw a whole picture with a partial refresh: quick, no flashing.
    pub fn partial(&mut self, bytes: &[u8]) {
        self.partial_registers();
        let p = &mut self.panel;
        p.command(0x91); // partial in
        p.command(0x90); // the window: the whole panel
        p.data(&[0, WIDTH as u8 - 1, 0, 0, (HEIGHT / 256) as u8, (HEIGHT % 256 - 1) as u8, 0x28]);
        p.command(0x13);
        p.data(&bytes[..BYTES.min(bytes.len())]);
        self.refresh(Duration::from_millis(600));
    }

    /// Power down (the picture stays).
    pub fn sleep(&mut self) {
        let p = &mut self.panel;
        p.command(0x50);
        p.data(&[0xf7]);
        p.command(0x02); // power off
        p.command(0x07); // deep sleep
        p.data(&[0xa5]);
    }
}

/// The Centaur's panel on its Raspberry Pi: SPI 0.0 at 4 MHz, and BCM
/// GPIO 12 (reset), 16 (data/command), 18 (chip select), 13 (busy).
#[cfg(target_os = "linux")]
pub mod pi {
    use std::io::{self, Write};
    use std::time::Duration;

    use gpiocdev::Request;
    use gpiocdev::line::Value;
    use spidev::{SpiModeFlags, Spidev, SpidevOptions};

    use super::Panel;

    const RST: u32 = 12;
    const DC: u32 = 16;
    const CS: u32 = 18;
    const BUSY: u32 = 13;

    pub struct Pi {
        spi: Spidev,
        lines: Request,
    }

    impl Pi {
        pub fn open() -> io::Result<Pi> {
            let mut spi = Spidev::open("/dev/spidev0.0")?;
            spi.configure(
                &SpidevOptions::new()
                    .bits_per_word(8)
                    .max_speed_hz(4_000_000)
                    .mode(SpiModeFlags::SPI_MODE_0)
                    .build(),
            )?;
            let lines = Request::builder()
                .on_chip("/dev/gpiochip0")
                .with_consumer("open-chess e-paper")
                .with_lines(&[RST, DC, CS])
                .as_output(Value::Active)
                .with_line(BUSY)
                .as_input()
                .request()
                .map_err(io::Error::other)?;
            Ok(Pi { spi, lines })
        }

        fn set(&self, line: u32, high: bool) {
            let _ = self.lines.set_value(line, if high { Value::Active } else { Value::Inactive });
        }

        fn send(&mut self, dc: bool, bytes: &[u8]) {
            self.set(DC, dc);
            self.set(CS, false);
            for chunk in bytes.chunks(4096) {
                let _ = self.spi.write_all(chunk);
            }
            self.set(CS, true);
        }
    }

    impl Panel for Pi {
        fn reset(&mut self) {
            for _ in 0..3 {
                self.set(RST, true);
                std::thread::sleep(Duration::from_millis(20));
                self.set(RST, false);
                std::thread::sleep(Duration::from_millis(5));
            }
            self.set(RST, true);
            std::thread::sleep(Duration::from_millis(20));
        }

        fn command(&mut self, c: u8) {
            self.send(false, &[c]);
        }

        fn data(&mut self, d: &[u8]) {
            self.send(true, d);
        }

        fn wait(&mut self, d: Duration) {
            std::thread::sleep(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        log: Vec<(bool, Vec<u8>)>,
        resets: usize,
    }

    impl Panel for Recorder {
        fn reset(&mut self) {
            self.resets += 1;
        }
        fn command(&mut self, c: u8) {
            self.log.push((false, vec![c]));
        }
        fn data(&mut self, d: &[u8]) {
            self.log.push((true, d.to_vec()));
        }
        fn wait(&mut self, _: Duration) {}
    }

    #[test]
    fn sends_waveshares_sequences() {
        let mut e = Epd::new(Recorder::default());
        e.init();
        let commands: Vec<u8> = e.panel().log.iter().filter(|(d, _)| !d).map(|(_, b)| b[0]).collect();
        assert_eq!(commands, [0x04, 0x00, 0x61, 0x50]);
        assert_eq!(e.panel().log[4], (true, vec![0x80, 0x01, 0x28]));

        let picture = vec![0xaa; 4736];
        let mut e = Epd::new(Recorder::default());
        e.partial(&picture);
        let log = &e.panel().log;
        let window = log.iter().position(|(d, b)| !d && b[0] == 0x90).unwrap();
        assert_eq!(log[window + 1], (true, vec![0, 127, 0, 0, 1, 39, 0x28]));
        assert_eq!(log[log.len() - 2], (true, picture));
        assert_eq!(log[log.len() - 1], (false, vec![0x12]));
        let lut_bw = log.iter().position(|(d, b)| !d && b[0] == 0x22).unwrap();
        assert_eq!(log[lut_bw + 1].1[..3], [0x80, 0x19, 0x01]);
    }
}
