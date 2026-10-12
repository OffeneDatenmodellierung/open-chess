// The board: renders a FEN, highlights, arrows, and tap-to-move input.
// It knows nothing about rules; the app passes in the legal moves.

const GLYPH = { k: "♚", q: "♛", r: "♜", b: "♝", n: "♞", p: "♟" };
const NAME = { k: "king", q: "queen", r: "rook", b: "bishop", n: "knight", p: "pawn" };
const FILES = "abcdefgh";

export function parseFen(fen) {
  const rows = fen.split(" ")[0].split("/");
  const squares = {};
  rows.forEach((row, i) => {
    let f = 0;
    for (const c of row) {
      if (/\d/.test(c)) f += Number(c);
      else {
        squares[FILES[f] + (8 - i)] = { type: c.toLowerCase(), white: c === c.toUpperCase() };
        f += 1;
      }
    }
  });
  return squares;
}

export class Board {
  constructor(root, { onMove, onSelect } = {}) {
    this.root = root;
    this.onMove = onMove;
    this.onSelect = onSelect;
    // When set, taps go here instead of moving pieces (the board editor).
    this.onEditTap = null;
    this.white = true; // orientation: White at the bottom
    this.fen = "";
    this.legal = []; // [{uci, ...}]
    this.enabled = true;
    this.selected = null;
    this.last = null;
    this.check = null;
    this.arrows = [];
    this.root.classList.add("board");
    this.root.setAttribute("role", "grid");
    this.grid = document.createElement("div");
    this.grid.className = "grid";
    this.svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    this.svg.setAttribute("viewBox", "0 0 8 8");
    this.svg.setAttribute("class", "arrows");
    this.svg.setAttribute("aria-hidden", "true");
    this.root.append(this.grid, this.svg);
    this.squares = {};
    this.build();
  }

  build() {
    this.grid.textContent = "";
    this.squares = {};
    for (let r = 0; r < 8; r++) {
      for (let c = 0; c < 8; c++) {
        const file = this.white ? FILES[c] : FILES[7 - c];
        const rank = this.white ? 8 - r : r + 1;
        const sq = file + rank;
        const b = document.createElement("button");
        b.type = "button";
        b.className = "sq " + ((FILES.indexOf(file) + rank) % 2 === 0 ? "light" : "dark");
        b.dataset.sq = sq;
        if (c === 0) b.dataset.rank = rank;
        if (r === 7) b.dataset.file = file;
        b.addEventListener("click", () => this.tap(sq));
        this.grid.append(b);
        this.squares[sq] = b;
      }
    }
  }

  flip(white = !this.white) {
    if (white === this.white) return;
    this.white = white;
    this.build();
    this.render();
  }

  set({ fen, legal, last, check, enabled }) {
    if (fen !== undefined) this.fen = fen;
    if (legal !== undefined) this.legal = legal;
    if (last !== undefined) this.last = last;
    if (check !== undefined) this.check = check;
    if (enabled !== undefined) this.enabled = enabled;
    if (this.selected && !this.legal.some((m) => m.uci.startsWith(this.selected))) this.selected = null;
    this.render();
  }

  setArrows(arrows) {
    this.arrows = arrows;
    this.drawArrows();
  }

  render() {
    const pieces = parseFen(this.fen || "8/8/8/8/8/8/8/8 w - - 0 1");
    const targets = new Map();
    if (this.selected) {
      for (const m of this.legal) if (m.uci.startsWith(this.selected)) targets.set(m.uci.slice(2, 4), m);
    }
    for (const [sq, el] of Object.entries(this.squares)) {
      const p = pieces[sq];
      el.textContent = "";
      el.classList.toggle("last", !!this.last && (this.last.slice(0, 2) === sq || this.last.slice(2, 4) === sq));
      el.classList.toggle("selected", this.selected === sq);
      el.classList.toggle("target", targets.has(sq));
      el.classList.toggle("capture", targets.has(sq) && !!p);
      el.classList.toggle("in-check", this.check === sq);
      el.classList.toggle("mark", !!this.marks?.has(sq));
      let label = sq;
      if (p) {
        const span = document.createElement("span");
        span.className = "piece " + (p.white ? "w" : "b");
        span.textContent = GLYPH[p.type] + "︎";
        el.append(span);
        label = `${p.white ? "White" : "Black"} ${NAME[p.type]} on ${sq}`;
      }
      if (targets.has(sq)) label += ", legal destination";
      el.setAttribute("aria-label", label);
    }
    this.drawArrows();
  }

  center(sq) {
    const f = FILES.indexOf(sq[0]);
    const r = Number(sq[1]);
    const x = this.white ? f : 7 - f;
    const y = this.white ? 8 - r : r - 1;
    return [x + 0.5, y + 0.5];
  }

  drawArrows() {
    this.svg.textContent = "";
    const ns = "http://www.w3.org/2000/svg";
    const defs = document.createElementNS(ns, "defs");
    this.arrows.forEach((a, i) => {
      const m = document.createElementNS(ns, "marker");
      m.setAttribute("id", `ah${i}`);
      m.setAttribute("viewBox", "0 0 10 10");
      m.setAttribute("refX", "5");
      m.setAttribute("refY", "5");
      m.setAttribute("markerWidth", "3");
      m.setAttribute("markerHeight", "3");
      m.setAttribute("orient", "auto");
      const p = document.createElementNS(ns, "path");
      p.setAttribute("d", "M0 0 L10 5 L0 10 z");
      p.setAttribute("class", `arrow-head ${a.kind || ""}`);
      m.append(p);
      defs.append(m);
    });
    this.svg.append(defs);
    this.arrows.forEach((a, i) => {
      const [x1, y1] = this.center(a.uci.slice(0, 2));
      const [x2, y2] = this.center(a.uci.slice(2, 4));
      const len = Math.hypot(x2 - x1, y2 - y1) || 1;
      const k = (len - 0.35) / len;
      const line = document.createElementNS(ns, "line");
      line.setAttribute("x1", x1);
      line.setAttribute("y1", y1);
      line.setAttribute("x2", x1 + (x2 - x1) * k);
      line.setAttribute("y2", y1 + (y2 - y1) * k);
      line.setAttribute("class", `arrow ${a.kind || ""}`);
      line.setAttribute("marker-end", `url(#ah${i})`);
      this.svg.append(line);
    });
  }

  tap(sq) {
    if (this.onEditTap) return this.onEditTap(sq);
    if (!this.enabled) return;
    const from = this.selected;
    if (from && from !== sq) {
      const moves = this.legal.filter((m) => m.uci.startsWith(from + sq));
      if (moves.length === 1) {
        this.selected = null;
        this.onMove?.(moves[0].uci);
        return;
      }
      if (moves.length > 1) {
        this.selected = null;
        this.render();
        this.choosePromotion(sq, moves).then((uci) => uci && this.onMove?.(uci));
        return;
      }
    }
    const own = this.legal.some((m) => m.uci.startsWith(sq));
    this.selected = own && from !== sq ? sq : null;
    this.onSelect?.(this.selected);
    this.render();
  }

  choosePromotion(sq, moves) {
    return new Promise((resolve) => {
      const dlg = document.createElement("dialog");
      dlg.className = "promo";
      const white = parseFen(this.fen)[moves[0].uci.slice(0, 2)]?.white;
      const h = document.createElement("p");
      h.textContent = `Promote the pawn on ${sq} to`;
      dlg.append(h);
      for (const m of moves) {
        const t = m.uci[4];
        const b = document.createElement("button");
        b.type = "button";
        b.innerHTML = `<span class="piece ${white ? "w" : "b"}">${GLYPH[t]}︎</span><span>${NAME[t]}</span>`;
        b.addEventListener("click", () => {
          dlg.close();
          resolve(m.uci);
        });
        dlg.append(b);
      }
      dlg.addEventListener("close", () => {
        dlg.remove();
        resolve(null);
      });
      document.body.append(dlg);
      dlg.showModal();
    });
  }
}

export { GLYPH, NAME };
