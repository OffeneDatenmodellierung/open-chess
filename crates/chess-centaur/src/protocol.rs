//! The DGT Centaur's board controller protocol, without I/O.
//!
//! The Centaur's Raspberry Pi talks to the board's controller on its
//! serial port (`/dev/serial0`, 1,000,000 baud). Every packet is a command
//! byte, often a 14-bit length (two 7-bit bytes, counting the whole packet),
//! the controller's two-byte address, data, and a checksum: the sum of the
//! bytes before it, modulo 128. The controller's address is learned at
//! start (see [`ask_address`]).
//!
//! Squares are numbered here from a1 (0) to h8 (63), rank by rank; the
//! controller numbers them from a8 (0) to h1 (63).
//!
//! What's known of the protocol comes from the DGTCentaurMods project
//! (GPL-3.0), which worked it out.

/// The controller's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Address(pub u8, pub u8);

/// The checksum: the sum of the bytes, modulo 128.
pub fn checksum(bytes: &[u8]) -> u8 {
    (bytes.iter().map(|&b| u32::from(b)).sum::<u32>() % 128) as u8
}

/// Between our square numbers and the controller's (the same both ways).
pub fn flip(square: u8) -> u8 {
    (7 - square / 8) * 8 + square % 8
}

/// A command with no length: `[command, address, checksum]`.
fn bare(command: u8, a: Address) -> Vec<u8> {
    let mut p = vec![command, a.0, a.1];
    p.push(checksum(&p));
    p
}

/// A command with a length: `[command, length, address, data, checksum]`.
fn sized(command: u8, a: Address, data: &[u8]) -> Vec<u8> {
    let len = data.len() + 6;
    let mut p = vec![command, ((len >> 7) & 0x7f) as u8, (len & 0x7f) as u8, a.0, a.1];
    p.extend_from_slice(data);
    p.push(checksum(&p));
    p
}

/// Sent first, before asking for the address.
pub fn wake() -> [&'static [u8]; 2] {
    [&[0x4d], &[0x4e]]
}

/// Ask the controller for its address; the answer's bytes 3 and 4 are it.
pub fn ask_address() -> Vec<u8> {
    vec![0x87, 0x00, 0x00, 0x07]
}

/// Ask for pieces lifted and put down since last asked.
pub fn poll_fields(a: Address) -> Vec<u8> {
    bare(0x83, a)
}

/// Ask for buttons pressed.
pub fn poll_buttons(a: Address) -> Vec<u8> {
    bare(0x94, a)
}

/// Ask for the battery's level and the charger.
pub fn ask_battery(a: Address) -> Vec<u8> {
    bare(0x98, a)
}

/// Ask for every square's sensor reading.
pub fn ask_board(a: Address) -> Vec<u8> {
    sized(0xf0, a, &[0x7f])
}

/// Every square's light off.
pub fn leds_off(a: Address) -> Vec<u8> {
    sized(0xb0, a, &[0x00])
}

/// Light two squares, a move's from and to (`intensity` 1–10).
pub fn leds_from_to(a: Address, from: u8, to: u8, intensity: u8) -> Vec<u8> {
    sized(0xb0, a, &[0x05, 0x03, 0x00, intensity, flip(from), flip(to)])
}

/// Light squares, cycling at `speed`.
pub fn leds(a: Address, squares: &[u8], speed: u8, intensity: u8) -> Vec<u8> {
    let mut data = vec![0x05, speed, 0x00, intensity];
    data.extend(squares.iter().map(|&s| flip(s)));
    sized(0xb0, a, &data)
}

/// The board's sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    General,
    Factory,
    PowerOff,
    PowerOn,
    Wrong,
    WrongMove,
}

/// Play a sound.
pub fn beep(a: Address, sound: Sound) -> Vec<u8> {
    let data: &[u8] = match sound {
        Sound::General => &[0x4c, 0x08],
        Sound::Factory => &[0x4c, 0x40],
        Sound::PowerOff => &[0x4c, 0x08, 0x48, 0x08],
        Sound::PowerOn => &[0x48, 0x08, 0x4c, 0x08],
        Sound::Wrong => &[0x4e, 0x0c, 0x48, 0x10],
        Sound::WrongMove => &[0x48, 0x08],
    };
    sized(0xb1, a, data)
}

/// Put the controller to sleep.
pub fn sleep(a: Address) -> Vec<u8> {
    sized(0xb2, a, &[0x0a])
}

/// A piece lifted from or put down on a square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Lifted(u8),
    Placed(u8),
}

/// The Centaur's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Back,
    Tick,
    Up,
    Down,
    Help,
    Play,
}

impl Button {
    fn from_code(code: u8) -> Option<Button> {
        Some(match code {
            0x01 => Button::Back,
            0x10 => Button::Tick,
            0x08 => Button::Up,
            0x02 => Button::Down,
            0x40 => Button::Help,
            0x04 => Button::Play,
            _ => return None,
        })
    }
}

/// A sensor reading above this (and below [`OCCUPIED_MAX`]) is a piece.
pub const OCCUPIED_MIN: u16 = 300;
pub const OCCUPIED_MAX: u16 = 32000;

/// A packet from the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Pieces lifted and put down, in order.
    Fields(Vec<Field>),
    /// A button pressed.
    Button(Button),
    /// Nothing new (an idle answer to a poll).
    Idle,
    /// The battery: 0–20, and whether the charger is plugged in.
    Battery { level: u8, charging: bool },
    /// Which squares have a piece, as a bitboard (bit 0 = a1).
    Occupancy(u64),
    /// Anything else, with the address it carried.
    Other { command: u8, address: Address },
}

/// Splits the controller's byte stream into packets.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole packet, skipping noise.
    pub fn next_packet(&mut self) -> Option<Vec<u8>> {
        loop {
            let start = self.buf.iter().position(|&b| b & 0x80 != 0)?;
            self.buf.drain(..start);
            if self.buf.len() < 3 {
                return None;
            }
            let len = (usize::from(self.buf[1] & 0x7f) << 7) | usize::from(self.buf[2] & 0x7f);
            if !(6..=1024).contains(&len) || self.buf[1] & 0x80 != 0 || self.buf[2] & 0x80 != 0 {
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < len {
                return None;
            }
            if checksum(&self.buf[..len - 1]) == self.buf[len - 1] {
                return Some(self.buf.drain(..len).collect());
            }
            self.buf.remove(0);
        }
    }

    /// The next message.
    pub fn next_message(&mut self) -> Option<Message> {
        self.next_packet().map(|p| parse(&p))
    }
}

/// Read a whole packet (checked by the decoder).
pub fn parse(p: &[u8]) -> Message {
    let address = Address(p.get(3).copied().unwrap_or(0), p.get(4).copied().unwrap_or(0));
    let data = p.get(5..p.len().saturating_sub(1)).unwrap_or(&[]);
    match p[0] {
        0x85 if data.is_empty() => Message::Idle,
        0x85 => {
            let mut fields = Vec::new();
            for pair in data.chunks_exact(2) {
                match pair[0] {
                    0x40 if pair[1] < 64 => fields.push(Field::Lifted(flip(pair[1]))),
                    0x41 if pair[1] < 64 => fields.push(Field::Placed(flip(pair[1]))),
                    _ => {}
                }
            }
            Message::Fields(fields)
        }
        0xb1 if data.len() >= 5 && data[1..4] == [0x14, 0x0a, 0x05] => {
            Button::from_code(data[4]).map_or(Message::Idle, Message::Button)
        }
        0xb1 => Message::Idle,
        0xb5 if !data.is_empty() => {
            let charger = (data[0] >> 5) & 7;
            Message::Battery { level: data[0] & 31, charging: charger == 1 || charger == 2 }
        }
        _ if data.len() >= 129 => {
            let mut occupied = 0u64;
            for (i, v) in data[1..129].chunks_exact(2).enumerate() {
                let reading = u16::from_be_bytes([v[0], v[1]]);
                if (OCCUPIED_MIN..=OCCUPIED_MAX).contains(&reading) {
                    occupied |= 1 << flip(i as u8);
                }
            }
            Message::Occupancy(occupied)
        }
        command => Message::Other { command, address },
    }
}

/// A packet as the controller sends it (for tests and simulations).
pub fn from_controller(command: u8, a: Address, data: &[u8]) -> Vec<u8> {
    sized(command, a, data)
}

/// The controller's answer to [`ask_board`] for an occupancy (bit 0 = a1).
pub fn board_reading(a: Address, occupied: u64) -> Vec<u8> {
    let mut data = vec![0x7f];
    for i in 0..64u8 {
        let v: u16 = if occupied & (1 << flip(i)) != 0 { 2000 } else { 50 };
        data.extend_from_slice(&v.to_be_bytes());
    }
    sized(0xf0, a, &data)
}

/// The controller's answer to a fields poll.
pub fn fields_reading(a: Address, fields: &[Field]) -> Vec<u8> {
    let mut data = Vec::new();
    for f in fields {
        match *f {
            Field::Lifted(s) => data.extend_from_slice(&[0x40, flip(s)]),
            Field::Placed(s) => data.extend_from_slice(&[0x41, flip(s)]),
        }
    }
    sized(0x85, a, &data)
}

/// The controller's answer to a buttons poll, `None` for no button.
pub fn button_reading(a: Address, button: Option<Button>) -> Vec<u8> {
    let code = match button {
        None => return sized(0xb1, a, &[]),
        Some(Button::Back) => 0x01,
        Some(Button::Tick) => 0x10,
        Some(Button::Up) => 0x08,
        Some(Button::Down) => 0x02,
        Some(Button::Help) => 0x40,
        Some(Button::Play) => 0x04,
    };
    sized(0xb1, a, &[0x00, 0x14, 0x0a, 0x05, code, 0x00, 0x00, 0x00, 0x00])
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Address = Address(0x06, 0x50);

    #[test]
    fn packets_match_dgtcentaurmods() {
        // The idle answers DGTCentaurMods compares against: `85 00 06 a a cs`.
        let idle = from_controller(0x85, A, &[]);
        assert_eq!(&idle[..5], [0x85, 0x00, 0x06, 0x06, 0x50]);
        assert_eq!(idle[5], checksum(&idle[..5]));
        assert_eq!(leds_off(A)[..6], [0xb0, 0x00, 0x07, 0x06, 0x50, 0x00]);
        assert_eq!(beep(A, Sound::General)[..7], [0xb1, 0x00, 0x08, 0x06, 0x50, 0x4c, 0x08]);
        assert_eq!(ask_board(A)[..6], [0xf0, 0x00, 0x07, 0x06, 0x50, 0x7f]);
        assert_eq!(sleep(A)[..6], [0xb2, 0x00, 0x07, 0x06, 0x50, 0x0a]);
        // From e2 (12) to e4 (28): the controller's 52 and 36.
        let l = leds_from_to(A, 12, 28, 5);
        assert_eq!(l[..11], [0xb0, 0x00, 0x0c, 0x06, 0x50, 0x05, 0x03, 0x00, 0x05, 52, 36]);
        assert_eq!(poll_fields(A), [0x83, 0x06, 0x50, checksum(&[0x83, 0x06, 0x50])]);
        // A button answer: the bytes DGTCentaurMods matches for "back".
        let back = button_reading(A, Some(Button::Back));
        assert_eq!(back[..14], [0xb1, 0x00, 0x0f, 0x06, 0x50, 0x00, 0x14, 0x0a, 0x05, 0x01, 0, 0, 0, 0]);
    }

    #[test]
    fn decodes_the_controllers_answers() {
        let mut d = Decoder::default();
        d.push(&[0x00, 0x13]); // noise
        d.push(&fields_reading(A, &[Field::Lifted(12), Field::Placed(28)]));
        d.push(&button_reading(A, Some(Button::Tick)));
        d.push(&from_controller(0xb5, A, &[0b001_10001])); // charger 1, level 17
        let board = board_reading(A, 0xffff_0000_0000_ffff);
        d.push(&board[..50]);
        assert_eq!(d.next_message(), Some(Message::Fields(vec![Field::Lifted(12), Field::Placed(28)])));
        assert_eq!(d.next_message(), Some(Message::Button(Button::Tick)));
        assert_eq!(d.next_message(), Some(Message::Battery { level: 17, charging: true }));
        assert_eq!(d.next_message(), None, "half a packet");
        d.push(&board[50..]);
        assert_eq!(d.next_message(), Some(Message::Occupancy(0xffff_0000_0000_ffff)));
        assert_eq!(board.len(), 135);

        let mut bad = button_reading(A, Some(Button::Up));
        let last = bad.len() - 1;
        bad[last] ^= 1;
        d.push(&bad);
        d.push(&from_controller(0x85, A, &[]));
        assert_eq!(d.next_message(), Some(Message::Idle), "the damaged packet is skipped");
    }
}
