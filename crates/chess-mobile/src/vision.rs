//! The board reader: a position from a photo, on the device.

use chess_vision::{Image, Reader};

use crate::{MobileError, json};

fn vision(e: impl std::fmt::Display) -> MobileError {
    MobileError::Vision { message: e.to_string() }
}

/// A loaded board reader. Load it once; reading takes a few hundred
/// milliseconds on a phone's CPU, so call it off the main thread.
#[derive(uniffi::Object)]
pub struct BoardReader(Reader);

#[uniffi::export]
impl BoardReader {
    /// Load a weights file (a `.safetensors`). Weights trained on
    /// noncommercial data are for staff only and never ship, so the app
    /// refuses them as the Chess lab build does.
    #[uniffi::constructor]
    pub fn new(weights: Vec<u8>) -> Result<Self, MobileError> {
        let (_, card) = chess_vision::read_metadata(&weights).map_err(vision)?;
        if card.internal_only {
            return Err(MobileError::Vision {
                message: format!("{} is an internal-only model (trained on noncommercial data)", card.name),
            });
        }
        Reader::from_safetensors(&weights).map(BoardReader).map_err(vision)
    }

    /// Read RGBA pixels: a `BoardReading` as JSON (`placement`,
    /// `squares`, `confidence`, `unsureSquares`, `plausible`).
    pub fn read_json(&self, rgba: Vec<u8>, width: u32, height: u32) -> Result<String, MobileError> {
        let r = self
            .0
            .read(&Image { width: width as usize, height: height as usize, channels: 4, pixels: &rgba })
            .map_err(vision)?;
        Ok(json(&r))
    }

    /// The square side photos are scaled to; scale them near this first.
    pub fn image_size(&self) -> u32 {
        u32::try_from(self.0.config().image_size).unwrap_or(u32::MAX)
    }

    /// The model card (name, licence, datasets and their credits) as JSON,
    /// for the app's About screen.
    pub fn card_json(&self) -> String {
        json(self.0.card())
    }
}
