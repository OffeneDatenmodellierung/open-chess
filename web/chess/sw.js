// Offline cache for Chess lab. Everything the app needs, including the
// wasm engine and the font, is cached on install, so after one visit the
// bot and training work with no network. API calls (online play) are
// never cached. BUILD is replaced with the commit by the build step, so
// each release gets a fresh cache.
const BUILD = "__BUILD__";
const CACHE = `chess-lab-${BUILD}`;
const ASSETS = [
  "./",
  "index.html",
  "style.css",
  "app.js",
  "board.js",
  "online.js",
  "docs.js",
  "position-editor.js",
  "engine-worker.js",
  "manifest.webmanifest",
  "icon.svg",
  "icon-192.png",
  "icon-512.png",
  "icon-maskable-512.png",
  "fonts/atkinson-hyperlegible-latin-400-normal.woff2",
  "fonts/atkinson-hyperlegible-latin-700-normal.woff2",
  "pkg/open_chess_wasm.js",
  "pkg/open_chess_wasm_bg.wasm",
  "vision-worker.js",
  "pkg/open_chess_vision.js",
  "pkg/open_chess_vision_bg.wasm",
  // The board reader's weights (nets/board-reader.safetensors) are large
  // and optional: cached the first time a photo is read, not here.
];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(ASSETS)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("chess-lab-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin) return;
  if (/\/(api|oauth)(\/|$)/.test(url.pathname)) return;
  // The deployment's look can change without a release: the network first,
  // the cached copy offline.
  if (url.pathname.endsWith("/theme.css")) {
    e.respondWith(
      fetch(e.request)
        .then((res) => {
          if (res.ok) {
            const copy = res.clone();
            caches.open(CACHE).then((c) => c.put(e.request, copy));
          }
          return res;
        })
        .catch(() => caches.match(e.request).then((hit) => hit || new Response("", { headers: { "content-type": "text/css" } }))),
    );
    return;
  }
  // Cache first; the page itself ignores its query string (?code= after
  // sign-in), so it opens offline too.
  e.respondWith(
    caches.match(e.request, { ignoreSearch: e.request.mode === "navigate" }).then(
      (hit) =>
        hit ||
        fetch(e.request).then((res) => {
          if (res.ok && res.type === "basic") {
            const copy = res.clone();
            caches.open(CACHE).then((c) => c.put(e.request, copy));
          }
          return res;
        }),
    ),
  );
});
