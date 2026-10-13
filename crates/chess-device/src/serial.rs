//! A board's serial port, on Unix (termios, through `chess-dgt`) or
//! Windows (COM ports).

use std::io;

#[cfg(unix)]
pub struct Serial(chess_dgt::serial::Port);

#[cfg(unix)]
impl Serial {
    /// Open at a speed (DGT boards: 9600; the Centaur's controller:
    /// 1,000,000), raw 8N1, reads returning after a tenth of a second.
    pub fn open(path: &str, baud: u32) -> io::Result<Serial> {
        chess_dgt::serial::Port::open_at(path, baud).map(Serial)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }

    /// Bytes waiting, or none after the read timeout.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

#[cfg(windows)]
pub struct Serial(Box<dyn serialport::SerialPort>);

#[cfg(windows)]
impl Serial {
    pub fn open(path: &str, baud: u32) -> io::Result<Serial> {
        serialport::new(path, baud)
            .timeout(std::time::Duration::from_millis(100))
            .open()
            .map(Serial)
            .map_err(io::Error::other)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut self.0, bytes)?;
        io::Write::flush(&mut self.0)
    }

    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match io::Read::read(&mut self.0, buf) {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            r => r,
        }
    }
}

impl std::fmt::Debug for Serial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Serial")
    }
}
