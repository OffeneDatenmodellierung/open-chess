// Reads boards from photos on the device, off the main thread, with the
// Rust board reader (chess-vision) and a weights file next to the app
// (nets/board-reader.safetensors). Without that file, photos are read by
// the chess server instead. Once fetched, the service worker keeps
// the weights, so reading works offline.

import init, { BoardReader } from "./pkg/open_chess_vision.js";

let reader = null;
const ready = (async () => {
  try {
    const r = await fetch(new URL("./nets/board-reader.safetensors", import.meta.url));
    if (!r.ok) return;
    await init();
    reader = new BoardReader(new Uint8Array(await r.arrayBuffer()));
  } catch (e) {
    console.warn("board reader unavailable:", e);
  }
})();

self.onmessage = async (e) => {
  const { id, op, bitmap } = e.data;
  try {
    await ready;
    let result;
    if (op === "info") {
      result = reader ? { available: true, size: reader.imageSize(), card: JSON.parse(reader.card()) } : { available: false };
    } else if (op === "read") {
      if (!reader) throw new Error("no board reader on this device");
      // Scale to twice the reader's size first, so less is copied into
      // wasm; the reader's own scaling does the rest. (Not byte-identical
      // to the server's path, but well within what training's blur and
      // compression augmentation covers.)
      const side = Math.max(bitmap.width, bitmap.height);
      const scale = Math.min(1, (reader.imageSize() * 2) / side);
      const w = Math.max(1, Math.round(bitmap.width * scale));
      const h = Math.max(1, Math.round(bitmap.height * scale));
      const canvas = new OffscreenCanvas(w, h);
      const ctx = canvas.getContext("2d");
      ctx.drawImage(bitmap, 0, 0, w, h);
      const px = ctx.getImageData(0, 0, w, h).data;
      result = JSON.parse(reader.read(new Uint8Array(px.buffer, px.byteOffset, px.byteLength), w, h));
    } else {
      throw new Error(`unknown op ${op}`);
    }
    self.postMessage({ id, result });
  } catch (err) {
    self.postMessage({ id, error: String(err?.message || err) });
  }
};
