//! Read a board from a photo with a weights file, and time it.
//!
//!     cargo run --release -p open-chess-vision --features decode \
//!         --example read_photo -- board-reader.safetensors photo.jpg

use std::time::Instant;

use open_chess_vision::Reader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [weights, photo] = args.as_slice() else {
        return Err("usage: read_photo WEIGHTS PHOTO".into());
    };
    let t = Instant::now();
    let reader = Reader::from_safetensors(&std::fs::read(weights)?)?;
    eprintln!("loaded in {:?}: {:?}", t.elapsed(), reader.config());
    let t = Instant::now();
    let r = reader.read_encoded(&std::fs::read(photo)?)?;
    println!("{} in {:?}", r.placement, t.elapsed());
    println!("confidence {:.3}, plausible {}, unsure: {:?}", r.confidence, r.plausible, r.unsure_squares);
    Ok(())
}
