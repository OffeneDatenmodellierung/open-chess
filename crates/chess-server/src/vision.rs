//! Reading a chess board from a photo.
//!
//! Two readers, used in this order:
//!
//! - **Our board reader** (`chess-vision`, CHESS_VISION_LOCAL_MODEL): a
//!   network trained for exactly this, run on this server's CPU. It gives
//!   each square's piece probabilities, so the app can weigh every legal
//!   move against them.
//! - **A vision language model** (CHESS_VISION_ENDPOINT): asked for the
//!   piece placement as FEN, which side of the board was nearer the
//!   camera, the side to move if the picture shows it, and which squares
//!   it wasn't sure of. Used when there is no board reader, or for its
//!   second opinion on a photo the reader isn't sure of; that answer is
//!   also kept as the *suggestion* the Review page pre-fills.
//!
//! Either answer is a *reading*, not a position: the app matches it
//! against the legal moves from the game it is following, or shows it in a
//! board editor to confirm (see `chess_analysis::photo`).

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;

const PROMPT: &str = "You are reading a chess position from a photo or screenshot of a board. \
Reply with only a JSON object, no other text: \
{\"placement\": the FEN piece placement, ranks 8 down to 1 separated by '/', White's pieces as \
uppercase letters (KQRBNP) and Black's as lowercase, empty squares as digits, always written from \
White's point of view so rank 1 is White's back rank whichever way the photo was taken; \
\"white_at_bottom\": true if White's pieces were at the bottom of the image (nearer the camera), false if \
Black's were; \"side_to_move\": \"w\" or \"b\" only if the image shows it (a move indicator, a \
highlighted last move, a clock), otherwise null; \"confidence\": your confidence from 0 to 1; \
\"unsure_squares\": the squares (like \"e4\") you could not read reliably}. \
Count each rank's squares carefully so every rank adds up to eight. \
If the image does not show a chess board, reply {\"error\": \"what it shows instead\"}.";

/// What the model read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    pub placement: String,
    #[serde(default = "yes")]
    pub white_at_bottom: bool,
    #[serde(default)]
    pub side_to_move: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub unsure_squares: Vec<String>,
}

fn yes() -> bool {
    true
}

pub(crate) fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Whether the vision language model is set up.
pub fn enabled(cfg: &Config) -> bool {
    !cfg.vision_endpoint.trim().is_empty()
}

/// Load the board reader named by CHESS_VISION_LOCAL_MODEL (`None` when
/// it's empty).
pub fn load_reader(cfg: &Config) -> Result<Option<chess_vision::Reader>> {
    let path = cfg.vision_local_model.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading CHESS_VISION_LOCAL_MODEL ({path})"))?;
    let r = chess_vision::Reader::from_safetensors(&bytes)
        .with_context(|| format!("loading the board reader {path}"))?;
    Ok(Some(r))
}

/// The bytes and media type of a `data:image/...;base64,` URL or bare
/// base64 (taken as JPEG).
pub fn decode_image(image: &str) -> Result<(Vec<u8>, String)> {
    use base64::Engine as _;
    let (mime, b64) = match image.strip_prefix("data:") {
        Some(rest) => {
            let (head, data) = rest.split_once(',').context("a data: URL without a comma")?;
            let mime = head.split(';').next().unwrap_or("image/jpeg");
            if !head.ends_with(";base64") {
                bail!("the photo's data: URL isn't base64");
            }
            (mime.to_owned(), data)
        }
        None => ("image/jpeg".to_owned(), image),
    };
    if !matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp") {
        bail!("send the photo as JPEG, PNG or WebP (not {mime})");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("the photo isn't valid base64")?;
    Ok((bytes, mime))
}

/// Pull the JSON object out of a model reply (models sometimes wrap it in
/// prose or a code fence).
fn json_in(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// Parse and sanity-check a reply.
pub fn parse_reply(text: &str) -> Result<Reading> {
    let v = json_in(text).context("the vision model's reply had no JSON in it")?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("no chess board found: {e}");
    }
    let r: Reading =
        serde_json::from_value(v).context("the vision model's reply wasn't in the expected shape")?;
    if chess_analysis::photo::parse_placement(&r.placement).is_none() {
        bail!("the board read from the photo doesn't have eight ranks of eight squares ({})", r.placement);
    }
    Ok(r)
}

/// Ask the model to read `image` (a `data:` URL or bare base64 JPEG/PNG).
pub async fn read_board(cfg: &Config, image: &str) -> Result<Reading> {
    let url = if image.starts_with("data:") {
        image.to_string()
    } else {
        format!("data:image/jpeg;base64,{image}")
    };
    let body = json!({
        "model": cfg.vision_model,
        "max_tokens": 600,
        "temperature": 0,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "image_url", "image_url": { "url": url } },
                { "type": "text", "text": PROMPT },
            ],
        }],
    });
    let endpoint = format!("{}/chat/completions", cfg.vision_endpoint.trim_end_matches('/'));
    let mut req = http().post(&endpoint).json(&body);
    if let Some(k) = cfg.vision_api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.context("couldn't reach the vision model")?;
    let status = resp.status();
    let v: Value = resp.json().await.context("the vision model's answer wasn't JSON")?;
    if !status.is_success() {
        bail!(
            "the vision model refused the photo ({status}): {}",
            v["error"]["message"].as_str().unwrap_or("")
        );
    }
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    parse_reply(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_parsed_and_checked() {
        let r = parse_reply(
            "Here you go:\n```json\n{\"placement\": \"rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR\", \
             \"white_at_bottom\": false, \"side_to_move\": \"b\", \"confidence\": 0.8, \"unsure_squares\": [\"e4\"]}\n```",
        )
        .unwrap();
        assert!(!r.white_at_bottom);
        assert_eq!(r.side_to_move.as_deref(), Some("b"));
        assert_eq!(r.unsure_squares, vec!["e4"]);
        assert!(parse_reply("{\"error\": \"a cat\"}").unwrap_err().to_string().contains("a cat"));
        assert!(parse_reply("{\"placement\": \"8/8/8\"}").is_err());
        assert!(parse_reply("no idea").is_err());
    }

    #[test]
    fn photos_are_decoded() {
        let (b, m) = decode_image("data:image/png;base64,aGVsbG8=").unwrap();
        assert_eq!((b.as_slice(), m.as_str()), (&b"hello"[..], "image/png"));
        assert_eq!(decode_image("aGVsbG8=").unwrap().1, "image/jpeg");
        assert!(decode_image("data:image/gif;base64,aGVsbG8=").is_err());
        assert!(decode_image("data:image/png,hello").is_err());
        assert!(decode_image("data:image/png;base64,***").is_err());
    }
}
