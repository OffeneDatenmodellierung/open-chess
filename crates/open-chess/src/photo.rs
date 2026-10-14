//! `open-chess photo …`: read a chess position from a photo of a real
//! board, start an online game from it, or play the move a new photo shows
//! in a game that's running.
//!
//! The board reader is `chess-vision`: weights trained with
//! `open-chess train board-reader` (`--weights`), run here; or, without
//! weights, an open-chess server that reads photos (`--server` and
//! `--token`, its `POST /api/esl/recognise`). Either way a reading is each
//! square's piece probabilities, and a game being followed is matched move
//! by move against them (`chess_analysis::photo`), so a misread square or
//! two still finds the move.

use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use chess_analysis::photo::{
    CLASSES, Grid, ScoredCandidate, SquareProbs, argmax_grid, class_of, complete_fen, match_after_move_probs,
    parse_placement, placement,
};
use chess_client::{Api, ChallengeRequest, Client};
use chess_core::{Game, Side};
use chess_proto::BoardEvent;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

/// Played without asking when the reading gives the move at least this
/// chance (and stdin isn't a terminal to ask at).
const SURE: f64 = 0.9;

#[derive(Subcommand)]
pub enum Cmd {
    /// Read the position from a photo and show it (as a board and FEN).
    Read {
        #[command(flatten)]
        photo: PhotoArgs,
        #[command(flatten)]
        reader: ReaderArgs,
        #[command(flatten)]
        server: ServerArgs,
        /// Who is to move, for the FEN shown.
        #[arg(long, default_value = "white", value_parser = ["white", "black"])]
        turn: String,
        /// The reading as JSON (placement, FEN, each square's
        /// probabilities, confidence, unsure squares).
        #[arg(long)]
        json: bool,
    },
    /// Start an online game from the position in a photo: carry a game on
    /// a real board over to Lichess or an open-chess server.
    Start {
        #[command(flatten)]
        photo: PhotoArgs,
        #[command(flatten)]
        reader: ReaderArgs,
        #[command(flatten)]
        server: ServerArgs,
        /// Challenge this player.
        #[arg(long, conflicts_with = "computer", required_unless_present = "computer")]
        opponent: Option<String>,
        /// Play the server's computer at this level (1–8).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        computer: Option<u8>,
        /// Who is to move in the photo's position.
        #[arg(long, default_value = "white", value_parser = ["white", "black"])]
        turn: String,
        /// Castling still allowed, as a FEN writes it: any of K, Q, k and q, or - for none.
        /// Default: wherever king and rook are still at home.
        #[arg(long)]
        castling: Option<String>,
        /// The side you play.
        #[arg(long, default_value = "white", value_parser = ["white", "black", "random"])]
        colour: String,
        /// Clock: minutes+increment seconds (e.g. 10+5). Default: none.
        #[arg(long)]
        time: Option<String>,
        /// Start without asking to confirm the position.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Play the move a new photo shows in a game that's running: the photo
    /// is matched against every legal move from the game's position.
    Move {
        #[command(flatten)]
        photo: PhotoArgs,
        #[command(flatten)]
        reader: ReaderArgs,
        #[command(flatten)]
        server: ServerArgs,
        /// The game (default: your only game where it's your move).
        #[arg(long)]
        game: Option<String>,
        /// Play the best match without asking, however unsure.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Show the match without playing it.
        #[arg(long)]
        dry_run: bool,
        /// Then wait for the reply and say what to play on the board.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Args)]
pub struct PhotoArgs {
    /// The photo (JPEG, PNG or WebP), taken from any side of the board.
    #[arg(required_unless_present = "placement")]
    photo: Option<PathBuf>,
    /// The position's piece placement (FEN's first field) instead of a
    /// photo: for a board read by eye, or to try things out.
    #[arg(long, conflicts_with = "photo")]
    placement: Option<String>,
}

#[derive(Args)]
pub struct ReaderArgs {
    /// The board reader's weights (a .safetensors from `open-chess train
    /// board-reader`). Without them, the server reads the photo.
    #[arg(long, env = "OPEN_CHESS_BOARD_READER")]
    weights: Option<PathBuf>,
    /// Where the reader runs: auto, cpu, metal, cuda.
    #[arg(long, default_value = "auto")]
    compute: String,
}

#[derive(Args)]
pub struct ServerArgs {
    /// The server: lichess.org or an open-chess server.
    #[arg(long, env = "OPEN_CHESS_SERVER", default_value = "https://lichess.org")]
    server: String,
    /// Your API token there (board:play, challenge:write).
    #[arg(long, env = "OPEN_CHESS_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

impl ServerArgs {
    fn base(&self) -> String {
        self.server.trim().trim_end_matches('/').to_owned()
    }

    fn client(&self) -> Result<Client> {
        let Some(token) = self.token.as_deref().filter(|t| !t.trim().is_empty()) else {
            bail!("a token is needed for {} (--token or OPEN_CHESS_TOKEN)", self.base());
        };
        Ok(Client::new(&self.base(), Some(token.trim()))?)
    }
}

/// What was read, and from where.
#[derive(Debug, Clone)]
pub struct Reading {
    pub probs: SquareProbs,
    pub confidence: f32,
    pub unsure: Vec<String>,
    pub plausible: bool,
    /// "placement", "reader" (here) or "server".
    pub source: &'static str,
    /// The server's id for the photo when it kept it for review: the
    /// position played is sent back as its label.
    pub photo_id: Option<i64>,
}

impl Reading {
    pub fn grid(&self) -> Grid {
        argmax_grid(&self.probs)
    }

    /// A placement taken as read for sure.
    pub fn from_placement(text: &str, source: &'static str) -> Result<Reading> {
        let grid = parse_placement(text.split_whitespace().next().unwrap_or("")).context(
            "that isn't a piece placement: eight ranks of eight squares, like rnbqkbnr/pppppppp/8/…",
        )?;
        Ok(Reading {
            probs: one_hot(&grid, 0.98),
            confidence: 1.0,
            unsure: Vec::new(),
            plausible: chess_vision::plausible(&grid),
            source,
            photo_id: None,
        })
    }
}

/// Probabilities for a grid read with `p` certainty on every square.
pub fn one_hot(grid: &Grid, p: f32) -> SquareProbs {
    let rest = (1.0 - p) / 12.0;
    let mut out = [[rest; 13]; 64];
    for (sq, cell) in grid.iter().enumerate() {
        out[sq][class_of(*cell)] = p;
    }
    out
}

/// Read the board: the placement given, the reader here, or the server.
async fn read(photo: &PhotoArgs, reader: &ReaderArgs, server: &ServerArgs) -> Result<Reading> {
    if let Some(p) = &photo.placement {
        return Reading::from_placement(p, "placement");
    }
    let path = photo.photo.as_deref().context("name a photo, or give --placement")?;
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if let Some(w) = &reader.weights {
        return read_here(w, &reader.compute, bytes).await;
    }
    if server.token.is_some() {
        return read_on_server(server, path, &bytes).await;
    }
    bail!(
        "no board reader: give --weights (a board-reader .safetensors, from `open-chess train board-reader`), \
         or --server and --token for an open-chess server that reads photos"
    )
}

async fn read_here(weights: &Path, compute: &str, photo: Vec<u8>) -> Result<Reading> {
    let w = std::fs::read(weights).with_context(|| format!("reading {}", weights.display()))?;
    let compute = compute.to_owned();
    tokio::task::spawn_blocking(move || -> Result<Reading> {
        let device = chess_vision::device(&compute)?;
        let r = chess_vision::Reader::from_safetensors_on(&w, &device)?;
        let card = r.card();
        if !card.name.is_empty() {
            eprintln!(
                "board reader: {} {} ({})",
                card.name,
                card.version,
                chess_vision::device_name(&device)
            );
        }
        let b = r.read_encoded(&photo)?;
        Ok(Reading {
            probs: b.probs().context("the reader gave the wrong number of squares")?,
            confidence: b.confidence,
            unsure: b.unsure_squares,
            plausible: b.plausible,
            source: "reader",
            photo_id: None,
        })
    })
    .await?
}

async fn read_on_server(server: &ServerArgs, path: &Path, bytes: &[u8]) -> Result<Reading> {
    let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };
    let image = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    let url = format!("{}/api/esl/recognise", server.base());
    let r = reqwest::Client::new()
        .post(&url)
        .bearer_auth(server.token.as_deref().unwrap_or_default().trim())
        .json(&json!({ "image": image }))
        .send()
        .await
        .with_context(|| format!("reaching {url}"))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if status.as_u16() == 404 {
        bail!("{} doesn't read photos (it isn't an open-chess server): give --weights", server.base());
    }
    if !status.is_success() {
        let msg = v.get("error").and_then(Value::as_str).unwrap_or("no reason given");
        bail!("{} couldn't read the photo ({status}): {msg}", server.base());
    }
    reading_from_answer(&v)
}

/// The server's answer (`POST /api/esl/recognise`) as a reading.
#[allow(clippy::cast_possible_truncation)] // a probability, in an f32
pub fn reading_from_answer(v: &Value) -> Result<Reading> {
    let placement =
        v.get("placement").and_then(Value::as_str).context("no placement in the server's answer")?;
    let mut r = Reading::from_placement(placement, "server")?;
    let squares: Option<Vec<[f32; 13]>> =
        v.get("squares").and_then(|s| serde_json::from_value(s.clone()).ok());
    if let Some(p) = squares.and_then(|s| <SquareProbs>::try_from(s.as_slice()).ok()) {
        r.probs = p;
    } else {
        // A vision language model's reading: no probabilities, so less sure.
        r.probs = one_hot(&r.grid(), 0.9);
    }
    r.confidence =
        v.get("confidence").and_then(Value::as_f64).map_or(r.confidence, |c| c.clamp(0.0, 1.0) as f32);
    r.unsure =
        v.get("unsureSquares").and_then(|u| serde_json::from_value(u.clone()).ok()).unwrap_or_default();
    if let Some(p) = v.get("plausible").and_then(Value::as_bool) {
        r.plausible = p;
    }
    r.photo_id = v.get("photoId").and_then(Value::as_i64);
    Ok(r)
}

const GLYPHS: [char; 13] = ['·', '♙', '♘', '♗', '♖', '♕', '♔', '♟', '♞', '♝', '♜', '♛', '♚'];

/// The board as text, from White's side, unsure squares marked.
pub fn diagram(grid: &Grid, unsure: &[String]) -> String {
    let mut s = String::new();
    for rank in (0..8u8).rev() {
        let _ = write!(s, " {} ", rank + 1);
        for file in 0..8u8 {
            let sq = usize::from(rank * 8 + file);
            let name = format!("{}{}", (b'a' + file) as char, rank + 1);
            let glyph = GLYPHS[class_of(grid[sq])];
            s.push(glyph);
            s.push(if unsure.contains(&name) { '?' } else { ' ' });
        }
        s.push('\n');
    }
    s.push_str("   a b c d e f g h\n");
    s
}

/// A reading summed up in a line.
fn describe(r: &Reading) -> String {
    let mut s = match r.source {
        "reader" => "read by the board reader here".to_owned(),
        "server" => "read by the server".to_owned(),
        _ => "as given (no photo)".to_owned(),
    };
    if r.source != "placement" {
        let _ = write!(s, ", {:.0}% sure of every square", f64::from(r.confidence) * 100.0);
    }
    match r.unsure.len() {
        0 => {}
        1..=8 => {
            let _ = write!(s, "; unsure of {}", r.unsure.join(" "));
        }
        n => {
            let _ = write!(s, "; unsure of {n} squares");
        }
    }
    if !r.plausible {
        s.push_str("; not a possible game position as read");
    }
    s
}

/// Ask a yes/no question on the terminal (no, when there isn't one).
fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

/// Run a photo command.
pub fn run(cmd: Cmd) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(dispatch(cmd))
}

async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Read { photo, reader, server, turn, json } => {
            read_cmd(&photo, &reader, &server, &turn, json).await
        }
        Cmd::Start { photo, reader, server, opponent, computer, turn, castling, colour, time, yes } => {
            let opponent = match (computer, opponent) {
                (Some(level), _) => Opponent::Computer(level),
                (None, Some(name)) => Opponent::Player(name),
                (None, None) => bail!("--opponent or --computer"),
            };
            let position = Position { turn, castling };
            start_cmd(&photo, &reader, &server, &opponent, &position, colour, time.as_deref(), yes).await
        }
        Cmd::Move { photo, reader, server, game, yes, dry_run, wait } => {
            move_cmd(&photo, &reader, &server, game, yes, dry_run, wait).await
        }
    }
}

async fn read_cmd(
    photo: &PhotoArgs,
    reader: &ReaderArgs,
    server: &ServerArgs,
    turn: &str,
    json: bool,
) -> Result<()> {
    let r = read(photo, reader, server).await?;
    let grid = r.grid();
    let fen = complete_fen(&grid, turn == "white");
    if json {
        let classes: Vec<String> = CLASSES.iter().map(char::to_string).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "placement": placement(&grid),
                "fen": fen,
                "confidence": r.confidence,
                "unsureSquares": r.unsure,
                "plausible": r.plausible,
                "source": r.source,
                "photoId": r.photo_id,
                "classes": classes,
                "squares": r.probs.to_vec(),
            }))?
        );
    } else {
        print!("{}", diagram(&grid, &r.unsure));
        println!("{fen}");
        println!("{}", describe(&r));
    }
    Ok(())
}

struct Position {
    turn: String,
    castling: Option<String>,
}

enum Opponent {
    Player(String),
    Computer(u8),
}

#[allow(clippy::too_many_arguments)] // the command's options
async fn start_cmd(
    photo: &PhotoArgs,
    reader: &ReaderArgs,
    server: &ServerArgs,
    opponent: &Opponent,
    position: &Position,
    colour: String,
    time: Option<&str>,
    yes: bool,
) -> Result<()> {
    let client = server.client()?;
    let r = read(photo, reader, server).await?;
    let fen = start_fen(&r, &position.turn, position.castling.as_deref())?;
    print!("{}", diagram(&r.grid(), &r.unsure));
    println!("{fen}\n{}", describe(&r));
    if !yes && !confirm("Start the game from this position?") {
        bail!("not started (--yes starts without asking; --placement sets the position by hand)");
    }
    let (limit_secs, increment_secs) = match time {
        Some(t) => parse_time(t).map(|(l, i)| (Some(l), Some(i)))?,
        None => (None, None),
    };
    let req = ChallengeRequest {
        rated: false,
        limit_secs,
        increment_secs,
        color: Some(colour),
        fen: Some(fen),
        accept_by_token: None,
    };
    match opponent {
        Opponent::Computer(level) => {
            let id = client.challenge_ai(*level, &req).await?;
            println!("game {id} started against the computer (level {level}): {}", game_url(server, &id));
            println!("play your moves on the board, then: open-chess photo move <photo> --game {id}");
        }
        Opponent::Player(name) => {
            let c = client.challenge(name, &req).await?;
            println!("challenge sent to {name}: {}", c.url);
            println!("once they accept: open-chess photo move <photo> --game {}", c.id);
        }
    }
    Ok(())
}

async fn move_cmd(
    photo: &PhotoArgs,
    reader: &ReaderArgs,
    server: &ServerArgs,
    game: Option<String>,
    yes: bool,
    dry_run: bool,
    wait: bool,
) -> Result<()> {
    let client = server.client()?;
    let me = client.account().await?;
    let id = match game {
        Some(g) => g,
        None => my_turn_game(&client).await?,
    };
    let (game, mine) = game_now(&client, &id, &me.id).await?;
    if game.outcome().is_some() {
        bail!("game {id} is over");
    }
    if game.turn() != mine {
        bail!("it's your opponent's move in game {id}");
    }
    let r = read(photo, reader, server).await?;
    let candidates = match match_move(&game, &r) {
        Match::NoChange => {
            println!("the photo shows the position as it is: no move to play yet");
            return Ok(());
        }
        Match::MissingReply { last } => bail!(
            "the board doesn't show your opponent's last move ({last}) yet: make it on the board, then take the photo again"
        ),
        Match::Unclear { best } => bail!(
            "the photo doesn't match a legal move (the closest, {}, differs on {} squares): take it again, or check the board",
            best.san,
            best.distance
        ),
        Match::Move { candidates } => candidates,
    };
    let best = &candidates[0];
    println!("{} ({:.0}% likely)", best.san, best.probability * 100.0);
    for alt in candidates.iter().skip(1).take(2).filter(|c| c.probability >= 0.01) {
        println!("  or {} ({:.0}%)", alt.san, alt.probability * 100.0);
    }
    if dry_run {
        return Ok(());
    }
    // On a terminal, ask; otherwise play only a sure match (or with --yes).
    let play = if yes {
        true
    } else if std::io::stdin().is_terminal() {
        confirm(&format!("Play {}?", best.san))
    } else {
        best.probability >= SURE
    };
    if !play {
        bail!("not played: the match isn't sure enough (--yes plays the best match anyway)");
    }
    client.make_move(Api::Board, &id, &best.uci, false).await?;
    println!("played {} in game {id}", best.san);
    if let Some(photo_id) = r.photo_id {
        label_photo(server, photo_id, &best.fen).await;
    }
    if wait {
        wait_for_reply(&client, &id, game.ply() + 1).await?;
    }
    Ok(())
}

/// The FEN to start from: the reading, `turn` to move, with the castling
/// rights given (default: inferred from kings and rooks at home).
pub fn start_fen(r: &Reading, turn: &str, castling: Option<&str>) -> Result<String> {
    let grid = r.grid();
    if !r.plausible {
        bail!(
            "the photo doesn't read as a game position:\n{}take it again, or give the position with --placement",
            diagram(&grid, &r.unsure)
        );
    }
    let mut fen = complete_fen(&grid, turn == "white");
    if let Some(c) = castling {
        let c = c.trim();
        if c.is_empty() || !(c == "-" || c.chars().all(|ch| "KQkq".contains(ch))) {
            bail!("--castling: some of KQkq, or - for none");
        }
        let mut fields: Vec<&str> = fen.split(' ').collect();
        fields[2] = c;
        fen = fields.join(" ");
    }
    Game::new(Some(&fen)).map_err(|e| anyhow::anyhow!("{fen} can't start a game: {e}"))?;
    Ok(fen)
}

/// "10+5": ten minutes, five seconds a move (minutes may be ½, ¼ …).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked range
pub fn parse_time(t: &str) -> Result<(u32, u32)> {
    let (m, i) = t.split_once('+').unwrap_or((t, "0"));
    let mins: f64 = m.trim().parse().context("--time: minutes+seconds, like 10+5")?;
    if !(0.0..=180.0).contains(&mins) {
        bail!("--time: up to 180 minutes");
    }
    let inc: u32 = i.trim().parse().context("--time: minutes+seconds, like 10+5")?;
    Ok(((mins * 60.0).round() as u32, inc))
}

fn game_url(server: &ServerArgs, id: &str) -> String {
    format!("{}/{id}", server.base())
}

/// What a photo shows in a game.
#[derive(Debug)]
pub enum Match {
    /// The position as it is.
    NoChange,
    /// A legal move: the candidates, likeliest first.
    Move { candidates: Vec<ScoredCandidate> },
    /// The board is still at the position before the opponent's last
    /// move (its SAN), with our move made.
    MissingReply { last: String },
    /// Nothing near.
    Unclear { best: ScoredCandidate },
}

/// More squares off than this and the photo isn't of this game.
const FAR: usize = 4;

/// Match a reading against every legal move from the game's position.
pub fn match_move(game: &Game, r: &Reading) -> Match {
    let mut candidates = match_after_move_probs(game.board(), &r.probs);
    let Some(best) = candidates.first().cloned() else { return Match::Unclear { best: placeholder() } };
    if best.uci.is_empty() && best.distance <= 1 {
        return Match::NoChange;
    }
    // Perhaps the opponent's last move isn't on the board yet: the photo
    // shows the position before it, with or without our move made (our
    // move from there: a null move hands it back to us).
    if best.distance >= 2 && game.ply() > 0 {
        let mut before = game.clone();
        before.undo(1);
        let last = game.sans().last().cloned().unwrap_or_default();
        let closer = |b: &cozy_chess::Board| {
            match_after_move_probs(b, &r.probs)
                .first()
                .is_some_and(|c| c.distance <= 1 && c.distance < best.distance)
        };
        if before.board().null_move().is_some_and(|b| closer(&b)) {
            return Match::MissingReply { last };
        }
    }
    if best.distance <= FAR {
        candidates.retain(|c| !c.uci.is_empty());
        if candidates.is_empty() {
            return Match::NoChange;
        }
        return Match::Move { candidates };
    }
    Match::Unclear { best }
}

fn placeholder() -> ScoredCandidate {
    ScoredCandidate {
        uci: String::new(),
        san: "none".into(),
        fen: String::new(),
        distance: 64,
        log_likelihood: 0.0,
        probability: 0.0,
    }
}

/// Your one game where it's your move.
async fn my_turn_game(client: &Client) -> Result<String> {
    let playing = client.playing().await?;
    let mine: Vec<_> = playing.now_playing.iter().filter(|g| g.is_my_turn).collect();
    match mine.as_slice() {
        [one] => Ok(one.game_id.clone()),
        [] if playing.now_playing.is_empty() => bail!("you have no game running"),
        [] => bail!("it isn't your move in any of your games"),
        many => bail!(
            "it's your move in {} games: choose one with --game ({})",
            many.len(),
            many.iter()
                .map(|g| format!("{} v {}", g.game_id, g.opponent.username))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The game as it stands, and your side in it.
async fn game_now(client: &Client, id: &str, me: &str) -> Result<(Game, Side)> {
    let mut stream = client.stream_game(Api::Board, id).await?;
    while let Some(ev) = stream.next().await {
        if let BoardEvent::GameFull(full) = ev? {
            let side = if full.white.id.eq_ignore_ascii_case(me) {
                Side::White
            } else if full.black.id.eq_ignore_ascii_case(me) {
                Side::Black
            } else {
                bail!("you aren't playing in game {id}");
            };
            let initial = Some(full.initial_fen.as_str()).filter(|f| *f != "startpos" && !f.is_empty());
            let game = Game::from_moves(initial, &full.state.moves)
                .map_err(|e| anyhow::anyhow!("game {id}'s moves: {e}"))?;
            return Ok((game, side));
        }
    }
    bail!("game {id}: the server sent no game")
}

/// Wait for the opponent's move after ply `after` and say how to make it
/// on the board.
async fn wait_for_reply(client: &Client, id: &str, after: usize) -> Result<()> {
    println!("waiting for the reply…");
    let mut stream = client.stream_game(Api::Board, id).await?;
    let mut initial: Option<String> = None;
    while let Some(ev) = stream.next().await {
        let (moves, status) = match ev? {
            BoardEvent::GameFull(full) => {
                initial = Some(full.initial_fen.clone()).filter(|f| f != "startpos" && !f.is_empty());
                (full.state.moves.clone(), full.state.status)
            }
            BoardEvent::GameState(s) => (s.moves.clone(), s.status),
            _ => continue,
        };
        let game = Game::from_moves(initial.as_deref(), &moves).map_err(|e| anyhow::anyhow!("{e}"))?;
        if game.ply() > after {
            let uci = game.uci_moves().last().cloned().unwrap_or_default();
            let san = game.sans().last().cloned().unwrap_or_default();
            let (from, to) = (uci.get(0..2).unwrap_or(""), uci.get(2..4).unwrap_or(""));
            let who = if game.turn() == Side::White { "Black" } else { "White" };
            println!("{who} played {san}: on the board, move {from} to {to}");
            if let Some(o) = game.outcome() {
                println!("the game is over: {o:?}");
            }
            return Ok(());
        }
        if !status.is_ongoing() {
            println!("the game is over ({status:?})");
            return Ok(());
        }
    }
    bail!("the game's stream ended")
}

/// Tell the server what the photo showed (a label for training the next
/// reader). Best effort.
async fn label_photo(server: &ServerArgs, photo_id: i64, fen: &str) {
    let url = format!("{}/api/esl/photos/{photo_id}/confirm", server.base());
    let placement = fen.split_whitespace().next().unwrap_or("");
    let r = reqwest::Client::new()
        .post(&url)
        .bearer_auth(server.token.as_deref().unwrap_or_default().trim())
        .json(&json!({ "placement": placement, "fen": fen }))
        .send()
        .await;
    if let Err(e) = r.and_then(reqwest::Response::error_for_status) {
        eprintln!("(couldn't label the photo for training: {e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(moves: &str) -> Game {
        Game::from_moves(None, moves).unwrap()
    }

    fn reading_of(fen: &str) -> Reading {
        Reading::from_placement(fen, "placement").unwrap()
    }

    #[test]
    fn a_photo_after_a_move_finds_it() {
        let g = game("e2e4 e7e5");
        let r = reading_of("rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R");
        match match_move(&g, &r) {
            Match::Move { candidates } => {
                assert_eq!(candidates[0].uci, "g1f3");
                assert_eq!(candidates[0].san, "Nf3");
                assert!(candidates[0].probability > SURE);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_misread_square_still_finds_the_move() {
        let g = game("e2e4 e7e5");
        // Nf3 played, and the h7 pawn misread as a bishop.
        let r = reading_of("rnbqkbnr/pppp1ppb/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R");
        let Match::Move { candidates } = match_move(&g, &r) else { panic!() };
        assert_eq!(candidates[0].uci, "g1f3");
    }

    #[test]
    fn a_photo_from_blacks_side_reads_the_same() {
        let g = game("e2e4 e7e5");
        let mut grid = parse_placement("rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R").unwrap();
        grid.reverse();
        let r = Reading::from_placement(&placement(&grid), "placement").unwrap();
        let Match::Move { candidates } = match_move(&g, &r) else { panic!() };
        assert_eq!(candidates[0].uci, "g1f3");
    }

    #[test]
    fn no_move_and_a_missing_reply_are_told_apart() {
        let g = game("e2e4 e7e5");
        assert!(matches!(
            match_move(&g, &reading_of("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR")),
            Match::NoChange
        ));
        // White played Nf3 on the board, but Black's e5 isn't there yet.
        let r = reading_of("rnbqkbnr/pppppppp/8/8/4P3/5N2/PPPP1PPP/RNBQKB1R");
        match match_move(&g, &r) {
            Match::MissingReply { last } => assert_eq!(last, "e5"),
            other => panic!("{other:?}"),
        }
        let far = reading_of("8/8/8/8/8/8/8/K6k");
        assert!(matches!(match_move(&g, &far), Match::Unclear { .. }));
    }

    #[test]
    fn a_start_position_from_a_photo() {
        let r = reading_of("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R");
        let fen = start_fen(&r, "black", None).unwrap();
        assert_eq!(fen, "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R b KQkq - 0 1");
        // A king that has moved (and come back): no castling, as told.
        let fen = start_fen(&r, "white", Some("k")).unwrap();
        assert_eq!(fen, "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w k - 0 1");
        assert!(start_fen(&r, "white", Some("X")).is_err());
        assert!(start_fen(&reading_of("8/8/8/8/8/8/8/8"), "white", None).is_err());
    }

    #[test]
    fn the_servers_answer_reads() {
        let mut probs = vec![[0.0f32; 13]; 64];
        let grid = parse_placement("4k3/8/8/8/8/8/8/4K3").unwrap();
        for (sq, cell) in grid.iter().enumerate() {
            probs[sq][class_of(*cell)] = 1.0;
        }
        let v = json!({"placement": "4k3/8/8/8/8/8/8/4K3", "squares": probs, "confidence": 0.7,
                       "unsureSquares": ["e4"], "plausible": true, "photoId": 9});
        let r = reading_from_answer(&v).unwrap();
        assert_eq!((r.source, r.photo_id, r.unsure.clone()), ("server", Some(9), vec!["e4".to_owned()]));
        assert!((r.confidence - 0.7).abs() < 1e-6);
        // A language model's answer has no probabilities.
        let r = reading_from_answer(&json!({"placement": "4k3/8/8/8/8/8/8/4K3"})).unwrap();
        assert_eq!(placement(&r.grid()), "4k3/8/8/8/8/8/8/4K3");
    }

    #[test]
    fn times_and_diagrams() {
        assert_eq!(parse_time("10+5").unwrap(), (600, 5));
        assert_eq!(parse_time("3").unwrap(), (180, 0));
        assert!(parse_time("fast").is_err());
        let d = diagram(&parse_placement("4k3/8/8/8/8/8/8/4K3").unwrap(), &["e1".into()]);
        assert!(d.contains(" 1 · · · · ♔?· · · "), "{d}");
    }
}
