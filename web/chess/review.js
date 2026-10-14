// Checking board photos (#review): photos the board reader wasn't sure of,
// from anyone using this server, for anyone signed in to say what they
// show. Two matching answers label a photo (its sender's confirmation
// counts as one); an admin's or manager's answer is final. Labelled photos
// teach the next reader.
//
// Also the photo licence: asked once, the first time a photo of yours
// could be kept, and changeable here.

import { PositionEditor } from "./position-editor.js";

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const ASKED = "open-chess:photo-terms-asked";

let getApi = () => null;
let queue = [];
let staff = false;
let current = null;
let editor = null;
let imageUrl = null;

const view = () => document.getElementById("reviewView");

function store(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch {
    return null;
  }
}

/// The photo licence, in a dialog: accept (and optionally the public
/// dataset), or not now. Resolves to the terms state, or null.
export function askPhotoTerms(api, { force = false } = {}) {
  if (!api || (!force && store(ASKED))) return Promise.resolve(null);
  return new Promise((resolve) => {
    const dlg = document.createElement("dialog");
    dlg.className = "terms";
    dlg.innerHTML = `
      <h2>Help the board reader learn?</h2>
      <p>When the reader isn't sure of a photo, the server can keep it so people can say what it shows, and the next reader learns from it.</p>
      <p>If you agree, you give the server and the open-chess project a licence to use your board photos, and what people say they show, to train any model, commercial or not. People signed in to this server may see them to check them. Send only photos of a chess board, with nothing private in them.</p>
      <p><a href="#docs/photo-licence" target="_blank" rel="noopener">Read the photo licence</a></p>
      <label class="check"><input type="checkbox" id="termsPublic"> Also add my photos to the public dataset (CC0: anyone may use them)</label>
      <div class="row end">
        <button type="button" value="no">Not now</button>
        <button type="button" value="yes" class="primary">I agree</button>
      </div>`;
    const done = async (yes) => {
      store(ASKED, "1");
      dlg.close();
      dlg.remove();
      if (!yes) return resolve(null);
      try {
        resolve(await api.setPhotoTerms(true, dlg.querySelector("#termsPublic").checked));
      } catch (e) {
        console.warn("accepting the photo licence:", e);
        resolve(null);
      }
    };
    dlg.addEventListener("click", (e) => {
      const b = e.target.closest("button[value]");
      if (b) done(b.value === "yes");
    });
    dlg.addEventListener("cancel", () => done(false));
    document.body.append(dlg);
    dlg.showModal();
  });
}

function shell(body) {
  view().innerHTML = `<div class="review">
    <p><a class="back" href="#">← Back to the board</a></p>
    <h1>Board photos to check</h1>
    <p class="hint">Photos of real boards the board reader wasn't sure of, sent by people using this server. Set the board to what each photo shows and confirm: two matching answers teach the next reader.${staff ? " <b>You're an admin here: your answer is final.</b>" : ""}</p>
    ${body}
    <div id="termsBox" class="terms-box"></div>
  </div>`;
  renderTerms();
}

async function renderTerms() {
  const box = document.getElementById("termsBox");
  const api = getApi();
  if (!box || !api) return;
  try {
    const t = await api.photoTerms();
    box.innerHTML = t.accepted
      ? `<p class="hint">Your photos: kept under the <a href="#docs/photo-licence">photo licence</a>${t.public ? ", and in the public dataset (CC0)" : ""}.
         <button type="button" class="link" data-terms="public">${t.public ? "Leave the public dataset" : "Join the public dataset"}</button> ·
         <button type="button" class="link" data-terms="withdraw">Stop keeping my photos</button></p>`
      : `<p class="hint">Your photos aren't kept for training. <button type="button" class="link" data-terms="ask">Help the reader learn</button></p>`;
    box.onclick = async (e) => {
      const what = e.target.closest("[data-terms]")?.dataset.terms;
      if (!what) return;
      if (what === "ask") await askPhotoTerms(api, { force: true });
      else if (what === "withdraw") await api.setPhotoTerms(false, false);
      else await api.setPhotoTerms(true, !t.public);
      renderTerms();
    };
  } catch (e) {
    box.textContent = "";
  }
}

async function load() {
  const api = getApi();
  if (!api) {
    staff = false;
    return shell(`<p>Sign in to this server under <b>Online</b> to check photos.</p>`);
  }
  shell(`<p class="hint">Loading…</p>`);
  try {
    const r = await api.photos();
    staff = !!r.staff;
    queue = r.photos || [];
  } catch (e) {
    return shell(`<p class="error">Couldn't load the photos: ${esc(e.message)}</p>`);
  }
  next();
}

function unsureNote(sq) {
  if (!sq.length) return "";
  if (sq.length > 8) return `The reader wasn't sure of ${sq.length} squares (ringed): check the whole board.`;
  return `The reader wasn't sure of ${esc(sq.join(" "))} (ringed).`;
}

function guess(p) {
  return p.label || p.suggestion?.placement || p.reading?.placement || "8/8/8/8/8/8/8/8";
}

async function next() {
  if (imageUrl) URL.revokeObjectURL(imageUrl);
  imageUrl = null;
  current = queue.shift() || null;
  if (!current) return shell(`<p>Nothing to check right now. Thank you!</p>`);
  const p = current;
  const notes = [
    unsureNote(p.reading?.unsure_squares || []),
    p.label ? "The person who sent it says the board shows this." : p.suggestion ? "Filled in from a second reader's suggestion." : "",
    staff && p.votes ? `${p.votes} answer${p.votes === 1 ? "" : "s"} so far${p.disputed ? ", and they don't agree" : ""}.` : "",
  ].filter(Boolean).join(" ");
  shell(`
    <div class="review-card">
      <img id="reviewPhoto" alt="A photo of a chess board">
      <div>
        <div id="reviewEditor"></div>
        <p class="hint">${notes}</p>
        <p class="error" id="reviewError" role="alert"></p>
        <div class="row">
          <button type="button" class="primary" id="reviewConfirm">This is what it shows</button>
          <button type="button" id="reviewUnreadable">Not a readable board</button>
          <button type="button" id="reviewSkip">Skip</button>
        </div>
        <p class="hint">${queue.length} more after this one.</p>
      </div>
    </div>`);
  editor = new PositionEditor(document.getElementById("reviewEditor"), {
    placement: guess(p),
    unsure: p.reading?.unsure_squares || [],
  });
  document.getElementById("reviewConfirm").onclick = () => answer({ placement: editor.placement });
  document.getElementById("reviewUnreadable").onclick = () => answer({ unreadable: true });
  document.getElementById("reviewSkip").onclick = () => next();
  try {
    imageUrl = await getApi().photoImage(p.id);
    const img = document.getElementById("reviewPhoto");
    if (img && current === p) img.src = imageUrl;
  } catch (e) {
    console.warn(e);
  }
}

async function answer(body) {
  const err = document.getElementById("reviewError");
  try {
    const r = await getApi().labelPhoto(current.id, body);
    if (r.settled) console.info(`photo ${current.id} labelled`);
    next();
  } catch (e) {
    if (err) err.textContent = e.message;
  }
}

/// Wire it up: `api` returns the signed-in server's API (or null).
export function startReview({ api }) {
  getApi = api;
  addEventListener("open-chess:page", (e) => {
    if (e.detail === "review") load();
  });
}
