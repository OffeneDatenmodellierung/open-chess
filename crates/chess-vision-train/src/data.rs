//! Training data: each dataset's photos and labels, split into train,
//! validation and test without a game ever straddling two splits.
//!
//! Formats: ChessReD's own (`chessred`), our `labels.jsonl` (`jsonl`), and
//! folders of unlabelled photos (`photos`), which only a teacher reads.
//!
//! Labels are always the position from White's side (square 0 = a1),
//! however the photo was taken, so turning a photo never changes its
//! label. Mirroring would, so there's none.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chess_analysis::photo::{class_of, parse_placement};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::licences::Dataset;

/// 64 class indices (see `chess_analysis::photo::CLASSES`), a1 first.
pub type Labels = [u8; 64];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    Train,
    Val,
    Test,
}

impl Split {
    fn parse(s: &str) -> Option<Split> {
        match s {
            "train" => Some(Split::Train),
            "val" | "valid" | "validation" => Some(Split::Val),
            "test" => Some(Split::Test),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub image: PathBuf,
    /// All empty for an unlabelled photo (see `labelled`).
    pub labels: Labels,
    /// Whether `labels` is a person's (or a dataset's) reading. Unlabelled
    /// photos (format `photos`) learn only from a teacher's.
    pub labelled: bool,
    pub dataset: String,
    /// Photos of the same game share one, so a split never divides them.
    pub group: String,
    pub split: Split,
    /// A teacher's reading of the photo when distilling: its log
    /// probabilities, 64 squares × 13 classes (see `teacher.rs`).
    pub soft: Option<Arc<[f32]>>,
}

/// A FEN placement as labels.
pub fn labels_of(placement: &str) -> Result<Labels> {
    let grid = parse_placement(placement).with_context(|| format!("not a board: {placement:?}"))?;
    let mut out = [0u8; 64];
    for (sq, cell) in grid.iter().enumerate() {
        out[sq] = class_of(*cell) as u8;
    }
    Ok(out)
}

/// A stable 80/10/10 split by group, for data that comes without one.
pub fn split_of(group: &str) -> Split {
    let h = Sha256::digest(group.as_bytes());
    match u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 10 {
        0 => Split::Test,
        1 => Split::Val,
        _ => Split::Train,
    }
}

/// ChessReD's `annotations.json`: images with game ids, piece annotations
/// by algebraic square, categories by name, and its published splits
/// (by game).
fn load_chessred(d: &Dataset, root: &Path) -> Result<Vec<Sample>> {
    let base = root.join(&d.path);
    let ann: Value =
        serde_json::from_slice(&std::fs::read(base.join("annotations.json")).context("annotations.json")?)?;
    let mut cats: HashMap<i64, Option<u8>> = HashMap::new();
    for c in ann["categories"].as_array().context("no categories")? {
        let name = c["name"].as_str().unwrap_or_default().to_lowercase();
        let white = name.contains("white");
        let black = name.contains("black");
        let piece =
            ["pawn", "knight", "bishop", "rook", "queen", "king"].iter().position(|p| name.contains(p));
        let class = match (white, black, piece) {
            (true, false, Some(i)) => Some(1 + i as u8),
            (false, true, Some(i)) => Some(7 + i as u8),
            _ => None,
        };
        cats.insert(c["id"].as_i64().unwrap_or(-1), class);
    }
    let mut labels: HashMap<i64, Labels> = HashMap::new();
    for p in ann["annotations"]["pieces"].as_array().context("no piece annotations")? {
        let Some(Some(class)) = cats.get(&p["category_id"].as_i64().unwrap_or(-1)) else { continue };
        let sq = p["chessboard_position"].as_str().unwrap_or_default().as_bytes();
        if sq.len() != 2 || !(b'a'..=b'h').contains(&sq[0]) || !(b'1'..=b'8').contains(&sq[1]) {
            bail!("ChessReD: square {:?}", String::from_utf8_lossy(sq));
        }
        let idx = usize::from(sq[1] - b'1') * 8 + usize::from(sq[0] - b'a');
        labels.entry(p["image_id"].as_i64().unwrap_or(-1)).or_insert([0; 64])[idx] = *class;
    }
    let mut split_by_id = HashMap::new();
    for (name, split) in [("train", Split::Train), ("val", Split::Val), ("test", Split::Test)] {
        for id in ann["splits"][name]["image_ids"].as_array().into_iter().flatten() {
            split_by_id.insert(id.as_i64().unwrap_or(-1), split);
        }
    }
    let mut out = Vec::new();
    for img in ann["images"].as_array().context("no images")? {
        let id = img["id"].as_i64().unwrap_or(-1);
        let Some(&split) = split_by_id.get(&id) else { continue };
        let rel =
            img["path"].as_str().or_else(|| img["file_name"].as_str()).context("image without a path")?;
        let game = img["game_id"].as_i64().map_or(id.to_string(), |g| g.to_string());
        out.push(Sample {
            image: base.join(rel),
            labels: labels.get(&id).copied().unwrap_or([0; 64]),
            labelled: true,
            dataset: d.name.clone(),
            group: format!("{}:{game}", d.name),
            split,
            soft: None,
        });
    }
    Ok(out)
}

/// `labels.jsonl` lines: `{"image": path relative to the file, "placement":
/// FEN placement, "group": optional, "split": optional}`.
fn load_jsonl(d: &Dataset, root: &Path) -> Result<Vec<Sample>> {
    let base = root.join(&d.path);
    let text = std::fs::read_to_string(base.join("labels.jsonl"))
        .with_context(|| format!("{}/labels.jsonl", base.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let r: Value = serde_json::from_str(line).with_context(|| format!("labels.jsonl line {}", n + 1))?;
        let image = r["image"].as_str().with_context(|| format!("line {}: no image", n + 1))?;
        let placement = r["placement"].as_str().with_context(|| format!("line {}: no placement", n + 1))?;
        let group = format!("{}:{}", d.name, r["group"].as_str().unwrap_or(image));
        let split = r["split"].as_str().and_then(Split::parse).unwrap_or_else(|| split_of(&group));
        out.push(Sample {
            image: base.join(image),
            labels: labels_of(placement)?,
            labelled: true,
            dataset: d.name.clone(),
            group,
            split,
            soft: None,
        });
    }
    Ok(out)
}

/// Photo files under `dir`, at any depth, sorted (so a run can be repeated).
fn photo_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                todo.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// A folder of unlabelled board photos (format `photos`): training data
/// only, read by a teacher (`--teacher`). They have no labels to score
/// against, so they never go in a validation or test split.
fn load_photos(d: &Dataset, root: &Path) -> Result<Vec<Sample>> {
    let base = root.join(&d.path);
    let files = photo_files(&base)?;
    if files.is_empty() {
        bail!("{}: no photos (jpg, png, webp) under {}", d.name, base.display());
    }
    Ok(files
        .into_iter()
        .map(|image| {
            let rel = image.strip_prefix(&base).unwrap_or(&image).to_string_lossy().into_owned();
            Sample {
                image,
                labels: [0; 64],
                labelled: false,
                dataset: d.name.clone(),
                group: format!("{}:{rel}", d.name),
                split: Split::Train,
                soft: None,
            }
        })
        .collect())
}

pub fn load(d: &Dataset, root: &Path) -> Result<Vec<Sample>> {
    match d.format.as_str() {
        "chessred" => load_chessred(d, root),
        "jsonl" => load_jsonl(d, root),
        "photos" => load_photos(d, root),
        other => bail!("{}: unknown format {other:?} (chessred, jsonl or photos)", d.name),
    }
}

/// No game in more than one split.
pub fn check_no_leaks(samples: &[Sample]) -> Result<()> {
    let mut seen: HashMap<&str, Split> = HashMap::new();
    for s in samples {
        let first = *seen.entry(&s.group).or_insert(s.split);
        if first != s.split {
            bail!("{} appears in both {first:?} and {:?}", s.group, s.split);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(name: &str, format: &str) -> Dataset {
        Dataset { name: name.into(), format: format.into(), path: name.into(), ..Dataset::default() }
    }

    #[test]
    fn labels_and_splits() {
        let l = labels_of("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR").unwrap();
        assert_eq!(l[0], 4); // a1 white rook
        assert_eq!(l[4], 6); // e1 white king
        assert_eq!(l[60], 12); // e8 black king
        assert_eq!(l[27], 0);
        assert!(labels_of("8/8/8").is_err());
        assert_eq!(split_of("x:1"), split_of("x:1"));
        let n = 2000;
        let test = (0..n).filter(|i| split_of(&format!("g{i}")) == Split::Test).count();
        assert!((140..260).contains(&test), "{test}");
    }

    #[test]
    fn loads_jsonl_and_chessred() {
        let dir = std::env::temp_dir().join(format!("cvt-data-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ours")).unwrap();
        std::fs::write(
            dir.join("ours/labels.jsonl"),
            "{\"image\":\"a.jpg\",\"placement\":\"8/8/8/8/8/8/8/K6k\",\"group\":\"g1\",\"split\":\"val\"}\n\n\
             {\"image\":\"b.jpg\",\"placement\":\"8/8/8/8/8/8/8/K6k\",\"group\":\"g1\"}\n",
        )
        .unwrap();
        let s = load(&ds("ours", "jsonl"), &dir).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].split, s[0].labels[0], s[0].labels[7]), (Split::Val, 6, 12));
        // The second photo got a hashed split; same group as the first.
        assert!(check_no_leaks(&s).is_err() || s[1].split == Split::Val);

        std::fs::create_dir_all(dir.join("cr")).unwrap();
        std::fs::write(
            dir.join("cr/annotations.json"),
            r#"{"categories":[{"id":0,"name":"white-king"},{"id":1,"name":"black-queen"}],
                "images":[{"id":5,"file_name":"5.jpg","path":"images/0/5.jpg","game_id":0},{"id":6,"file_name":"6.jpg","game_id":1}],
                "annotations":{"pieces":[{"image_id":5,"category_id":0,"chessboard_position":"e1"},
                                          {"image_id":5,"category_id":1,"chessboard_position":"d8"}]},
                "splits":{"train":{"image_ids":[5]},"val":{"image_ids":[]},"test":{"image_ids":[6]}}}"#,
        )
        .unwrap();
        let s = load(&ds("cr", "chessred"), &dir).unwrap();
        assert_eq!(s.len(), 2);
        assert!(s[0].image.ends_with("images/0/5.jpg"));
        assert_eq!((s[0].labels[4], s[0].labels[59], s[0].split), (6, 11, Split::Train));
        assert_eq!((s[1].labels, s[1].split), ([0; 64], Split::Test));
        check_no_leaks(&s).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unlabelled_photos_are_training_only() {
        let dir = std::env::temp_dir().join(format!("cvt-photos-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("loose/sub")).unwrap();
        for f in ["loose/b.JPG", "loose/a.png", "loose/sub/c.webp", "loose/notes.txt"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let s = load(&ds("loose", "photos"), &dir).unwrap();
        let names: Vec<String> =
            s.iter().map(|x| x.image.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.png", "b.JPG", "c.webp"]);
        assert!(s.iter().all(|x| !x.labelled && x.split == Split::Train && x.soft.is_none()));
        check_no_leaks(&s).unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        assert!(load(&ds("empty", "photos"), &dir).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
