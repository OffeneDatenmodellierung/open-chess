//! The serial transport against a simulated board on a pseudo-terminal:
//! the real port setup, reads and writes, with a fake board answering on
//! the other end.

#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use open_chess_dgt::serial::{Flow, Port, run};
use open_chess_dgt::{Event, Follower, Observation, Session};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

fn frame(id: u8, data: &[u8]) -> Vec<u8> {
    let len = data.len() + 3;
    let mut v = vec![id | 0x80, (len >> 7) as u8, (len & 0x7f) as u8];
    v.extend_from_slice(data);
    v
}

/// A board dump: rows from rank 8 down, as piece codes.
fn dump(rows: [&[u8; 8]; 8]) -> Vec<u8> {
    let codes: Vec<u8> = rows.iter().flat_map(|r| r.iter().copied()).collect();
    frame(0x06, &codes)
}

const EMPTY: &[u8; 8] = &[0; 8];
const BLACK_BACK: &[u8; 8] = &[8, 9, 10, 12, 11, 10, 9, 8];
const BLACK_PAWNS: &[u8; 8] = &[7; 8];
const WHITE_PAWNS: &[u8; 8] = &[1; 8];
const WHITE_BACK: &[u8; 8] = &[2, 3, 4, 6, 5, 4, 3, 2];

#[test]
fn follows_a_move_on_a_simulated_board() {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_path = ptsname(&master, Vec::new()).unwrap().into_string().unwrap();
    let mut board_end = File::from(master);
    let mut board_reader = board_end.try_clone().unwrap();

    // The fake board: answers the version and board requests; after the
    // first board dump, a player plays e2-e4.
    thread::spawn(move || {
        let start = dump([BLACK_BACK, BLACK_PAWNS, EMPTY, EMPTY, EMPTY, EMPTY, WHITE_PAWNS, WHITE_BACK]);
        let after = dump([
            BLACK_BACK,
            BLACK_PAWNS,
            EMPTY,
            EMPTY,
            &[0, 0, 0, 0, 1, 0, 0, 0],
            EMPTY,
            &[1, 1, 1, 1, 0, 1, 1, 1],
            WHITE_BACK,
        ]);
        let mut dumps_sent = 0;
        let mut buf = [0u8; 64];
        loop {
            let Ok(n) = board_reader.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            for &b in &buf[..n] {
                let reply = match b {
                    0x4d => frame(0x13, &[1, 9]),
                    0x42 if dumps_sent == 0 => {
                        dumps_sent += 1;
                        let mut v = start.clone();
                        // e2 lifted (field 52), the pawn put on e4 (field 36).
                        v.extend(frame(0x0e, &[52, 0]));
                        v.extend(frame(0x0e, &[36, 1]));
                        v
                    }
                    0x42 => after.clone(),
                    _ => continue,
                };
                if board_end.write_all(&reply).is_err() {
                    return;
                }
            }
        }
    });

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut port = Port::open(&slave_path).expect("open the pty as a board port");
        let mut session = Session::new(Follower::new());
        let mut seen = Vec::new();
        let r = run(&mut port, &mut session, |e, _| {
            seen.push(e.clone());
            if matches!(e, Event::Game(Observation::Move { .. })) { Flow::Stop } else { Flow::Continue }
        });
        tx.send((r.map_err(|e| e.to_string()), seen)).unwrap();
    });

    let (result, seen) = rx.recv_timeout(Duration::from_secs(10)).expect("the session finished");
    result.unwrap();
    assert!(seen.contains(&Event::Connected { major: 1, minor: 9 }), "{seen:?}");
    assert_eq!(
        seen.last(),
        Some(&Event::Game(Observation::Move { uci: "e2e4".into(), san: "e4".into() })),
        "{seen:?}"
    );
}
