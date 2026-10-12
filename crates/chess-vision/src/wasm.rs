//! The board reader for the browser: the offline app's vision worker
//! loads the weights once and reads photos it has decoded with a canvas.

use wasm_bindgen::prelude::*;

use crate::{Image, Reader};

#[wasm_bindgen]
pub struct BoardReader(Reader);

#[wasm_bindgen]
impl BoardReader {
    /// Load a weights file (the bytes of a `.safetensors`).
    #[wasm_bindgen(constructor)]
    pub fn new(weights: &[u8]) -> Result<BoardReader, JsError> {
        Reader::from_safetensors(weights).map(BoardReader).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Read RGBA pixels (as from `getImageData`): a `BoardReading` as JSON.
    pub fn read(&self, rgba: &[u8], width: usize, height: usize) -> Result<String, JsError> {
        let r = self
            .0
            .read(&Image { width, height, channels: 4, pixels: rgba })
            .map_err(|e| JsError::new(&e.to_string()))?;
        serde_json::to_string(&r).map_err(|e| JsError::new(&e.to_string()))
    }

    /// The square side photos are scaled to (scale them near this before
    /// handing them over, to save copying).
    #[wasm_bindgen(js_name = imageSize)]
    pub fn image_size(&self) -> usize {
        self.0.config().image_size
    }

    /// The model card (name, licence, datasets) as JSON.
    pub fn card(&self) -> String {
        serde_json::to_string(self.0.card()).unwrap_or_default()
    }
}
