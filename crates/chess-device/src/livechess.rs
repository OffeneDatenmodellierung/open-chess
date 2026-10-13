//! DGT LiveChess's websocket, so Lichess's DGT page (lichess.org/dgt) plays
//! on any board the device drives.
//!
//! The page connects to `ws://localhost:1982/api/v1.0`, asks for the boards
//! (`eboards`), subscribes to the first one's events (`eboardevent`), and
//! after each move on Lichess tells the board the game's position
//! (`setup` with a FEN). The board's events carry its position and the
//! moves made since that setup (`san`), which the page sends to Lichess.
//!
//! [`LiveChess`] is that bookkeeping, without I/O: a setup one move on from
//! the board's game is the opponent's move, played on the board to wait
//! there (lit, on boards with lights); any other position starts the game
//! there. [`serve`] is the websocket. Browsers allow a plain websocket from
//! lichess.org only to `localhost`, so the page must run on the computer
//! the device runs on (the device's own screen, or a laptop driving a USB
//! or Bluetooth board).

use std::net::SocketAddr;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use chess_core::Game;
use chess_dgt::{Command, Square};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::board::Board;

/// The serial number the device reports for its board.
pub const SERIAL: &str = "OPENCHESS";

/// What the board's event feed says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Feed {
    /// The position's placement (FEN's first field).
    pub board: String,
    /// The moves made since the last setup, in SAN.
    pub san: Vec<String>,
}

/// The setups and the moves since.
#[derive(Debug, Default)]
pub struct LiveChess {
    /// The game's ply at the last setup.
    setup_ply: usize,
}

fn placement(game: &Game) -> String {
    game.fen().split(' ').next().unwrap_or("").to_owned()
}

impl LiveChess {
    pub fn new() -> LiveChess {
        LiveChess::default()
    }

    /// The page says the game is at `fen`.
    pub fn setup(&mut self, fen: &str, board: &mut impl Board) {
        let game = board.game();
        // Placement, side to move and castling: writers differ on when to
        // give an en passant square.
        let same = |g: &Game| g.fen().split(' ').take(3).eq(fen.split(' ').take(3));
        if same(game) {
            self.setup_ply = game.ply();
            return;
        }
        // One move on: the opponent's, to be made on the board.
        let next = game.legal_moves().into_iter().find(|m| {
            let mut g = game.clone();
            g.play_uci(m).is_ok() && same(&g)
        });
        if let Some(m) = next
            && board.play(&m).is_ok()
        {
            if let (Some(from), Some(to)) = (Square::from_name(&m[0..2]), Square::from_name(&m[2..4])) {
                board.send(Command::Lights { from, to });
            }
            self.setup_ply = board.game().ply();
            return;
        }
        // Anything else: the game starts again there.
        if board.set_position(fen).is_ok() {
            self.setup_ply = 0;
        }
    }

    /// The feed for the board's game now.
    pub fn feed(&self, board: &impl Board) -> Feed {
        let g = board.game();
        let san = if board.awaiting_board() {
            Vec::new()
        } else {
            g.sans().get(self.setup_ply.min(g.sans().len())..).unwrap_or(&[]).to_vec()
        };
        Feed { board: placement(g), san }
    }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// The answer to one of the page's messages, and whether it subscribed.
pub fn answer(msg: &Value, feed: &Feed) -> (Option<Value>, Option<i64>, Option<String>) {
    let id = msg.get("id").and_then(Value::as_i64).unwrap_or(0);
    match msg.get("call").and_then(Value::as_str) {
        Some("eboards") => (
            Some(json!({
                "response": "call",
                "id": id,
                "param": [{
                    "serialnr": SERIAL,
                    "source": "open-chess",
                    "state": "ACTIVE",
                    "battery": "100%",
                    "comment": null,
                    "board": feed.board,
                    "flipped": false,
                    "clock": null,
                }],
                "time": now_ms(),
            })),
            None,
            None,
        ),
        Some("subscribe") => {
            (Some(json!({"response": "call", "id": id, "param": null, "time": now_ms()})), Some(id), None)
        }
        Some("call") => {
            let param = msg.get("param");
            let method = param.and_then(|p| p.get("method")).and_then(Value::as_str);
            let fen = param.and_then(|p| p.get("param")).and_then(|p| p.get("fen")).and_then(Value::as_str);
            let reply = json!({"response": "call", "id": id, "param": true, "time": now_ms()});
            match (method, fen) {
                (Some("setup"), Some(fen)) => (Some(reply), None, Some(fen.to_owned())),
                _ => (Some(reply), None, None),
            }
        }
        _ => (None, None, None),
    }
}

/// A board event for a subscription.
pub fn event(subscription: i64, feed: &Feed) -> Value {
    json!({
        "response": "feed",
        "id": subscription,
        "param": {"serialnr": SERIAL, "flipped": false, "board": feed.board, "clock": null, "san": feed.san},
        "time": now_ms(),
    })
}

#[derive(Clone)]
struct Shared {
    feed: watch::Receiver<Feed>,
    setups: std_mpsc::Sender<String>,
    log: std_mpsc::Sender<String>,
}

/// Serve LiveChess's websocket on `addr` (normally `127.0.0.1:1982`) from
/// a thread of its own. Setups the page asks for arrive on `setups`; what
/// happens is told on `log`.
pub fn serve(
    addr: &str,
    feed: watch::Receiver<Feed>,
    setups: std_mpsc::Sender<String>,
    log: std_mpsc::Sender<String>,
) -> anyhow::Result<SocketAddr> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let app = Router::new().route("/api/v1.0", get(upgrade)).with_state(Shared { feed, setups, log });
    std::thread::Builder::new().name("livechess".into()).spawn(move || {
        rt.block_on(async move {
            if let Ok(l) = tokio::net::TcpListener::from_std(listener) {
                let _ = axum::serve(l, app).await;
            }
        });
    })?;
    Ok(local)
}

async fn upgrade(State(s): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| session(socket, s))
}

async fn session(mut socket: WebSocket, s: Shared) {
    let _ = s.log.send("livechess: a page connected".into());
    let mut rx = s.feed.clone();
    let mut subscription: Option<i64> = None;
    let mut last_sent: Option<Feed> = None;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(m)) = incoming else { break };
                let Message::Text(text) = m else { continue };
                let Ok(msg) = serde_json::from_str::<Value>(&text) else { continue };
                let _ = s.log.send(format!("livechess ← {text}"));
                let feed = rx.borrow().clone();
                let (reply, subscribed, setup) = answer(&msg, &feed);
                if let Some(fen) = setup {
                    let _ = s.setups.send(fen);
                }
                if let Some(r) = reply
                    && socket.send(Message::Text(r.to_string().into())).await.is_err()
                {
                    break;
                }
                if let Some(id) = subscribed {
                    subscription = Some(id);
                    let e = event(id, &feed);
                    if socket.send(Message::Text(e.to_string().into())).await.is_err() {
                        break;
                    }
                    last_sent = Some(feed);
                }
            }
            changed = rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let feed = rx.borrow_and_update().clone();
                if let Some(id) = subscription
                    && last_sent.as_ref() != Some(&feed)
                {
                    let e = event(id, &feed);
                    let _ = s.log.send(format!("livechess → {}", e["param"]));
                    if socket.send(Message::Text(e.to_string().into())).await.is_err() {
                        break;
                    }
                    last_sent = Some(feed);
                }
            }
            () = tokio::time::sleep(Duration::from_secs(30)) => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = s.log.send("livechess: the page went".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_dgt::{Follower, Session};

    #[test]
    fn answers_lichess_dgt_page() {
        let feed = Feed { board: "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR".into(), san: vec![] };
        let (r, sub, _) = answer(&json!({"id": 1, "call": "eboards"}), &feed);
        let r = r.unwrap();
        assert_eq!((r["response"].as_str(), r["id"].as_i64()), (Some("call"), Some(1)));
        assert_eq!(r["param"][0]["state"], "ACTIVE");
        assert_eq!(r["param"][0]["serialnr"], SERIAL);
        assert_eq!(sub, None);

        let (_, sub, _) = answer(
            &json!({"id": 2, "call": "subscribe", "param": {"feed": "eboardevent", "id": 1, "param": {"serialnr": SERIAL}}}),
            &feed,
        );
        assert_eq!(sub, Some(2));

        let fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1";
        let (_, _, setup) = answer(
            &json!({"id": 3, "call": "call", "param": {"id": 1, "method": "setup", "param": {"fen": fen}}}),
            &feed,
        );
        assert_eq!(setup.as_deref(), Some(fen));
        assert_eq!(event(2, &feed)["param"]["san"], json!([]));
    }

    /// The board shows the position after these moves.
    fn show(s: &mut Session, moves: &str) {
        let g = Game::from_moves(None, moves).unwrap();
        let p = chess_dgt::Placement::from_fen(&g.fen()).unwrap();
        let mut codes = vec![0x86, 0, 67];
        for i in 0..64 {
            let sq = (7 - i / 8) * 8 + i % 8;
            codes.push(p.0[sq].map_or(0, |piece| {
                let base = match piece.kind {
                    chess_dgt::PieceKind::Pawn => 1,
                    chess_dgt::PieceKind::Rook => 2,
                    chess_dgt::PieceKind::Knight => 3,
                    chess_dgt::PieceKind::Bishop => 4,
                    chess_dgt::PieceKind::King => 5,
                    chess_dgt::PieceKind::Queen => 6,
                };
                if piece.white { base } else { base + 6 }
            }));
        }
        s.receive(&codes, std::time::Instant::now());
    }

    #[test]
    fn moves_since_the_setup_go_to_the_page() {
        let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let mut s = Session::new(Follower::new());
        let mut lc = LiveChess::new();
        show(&mut s, "");
        lc.setup(start, &mut s);
        assert!(lc.feed(&s).san.is_empty());
        show(&mut s, "e2e4");
        assert_eq!(lc.feed(&s).san, ["e4"]);
        // Lichess replies e5: lit, waiting; then made on the board.
        lc.setup("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2", &mut s);
        assert!(lc.feed(&s).san.is_empty());
        show(&mut s, "e2e4 e7e5");
        assert!(lc.feed(&s).san.is_empty());
        show(&mut s, "e2e4 e7e5 g1f3");
        assert_eq!(lc.feed(&s).san, ["Nf3"]);
    }

    #[test]
    fn the_opponents_move_waits_on_the_board() {
        // After 1.e4 on the board, Lichess says the game is at 1.e4 e5.
        let mut s = Session::new(Follower::new());
        let mut lc = LiveChess::new();
        s.follower_mut().set_position("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        lc.setup("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1", &mut s);
        lc.setup("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2", &mut s);
        assert!(s.follower().awaiting_board());
        assert_eq!(s.follower().game().uci_moves(), ["e7e5"]);
        assert!(s.take_commands().iter().any(|c| matches!(c, Command::Lights { .. })));
        // Nothing to send while it waits.
        assert!(lc.feed(&s).san.is_empty());
        // A position from nowhere starts the game there.
        lc.setup("8/8/8/4k3/8/8/4K3/7Q w - - 0 1", &mut s);
        assert_eq!(s.follower().game().fen(), "8/8/8/4k3/8/8/4K3/7Q w - - 0 1");
        assert_eq!(lc.feed(&s).board, "8/8/8/4k3/8/8/4K3/7Q");
    }
}
