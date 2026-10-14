// The guides (docs/*.md, copied next to the app by the build) as a page of
// the app: #docs lists them, #docs/<name> shows one. The server sends
// /docs and /docs/<name> here too.

const GUIDES = [
  { id: "api", title: "The server API", blurb: "Lichess's API as open-chess speaks it, and what it adds." },
  { id: "lichess", title: "Lichess tools", blurb: "Using Lichess apps, bots and libraries with an open-chess server." },
  { id: "boards", title: "Electronic boards", blurb: "DGT, Chessnut, Centaur and DGT Pi boards with open-chess-device." },
  { id: "photos", title: "Boards from photos", blurb: "Read a position from a photo, carry a game online, play on from the board." },
  { id: "engine", title: "The engine", blurb: "Search, odds, explanations and training the engine." },
  { id: "releases", title: "Releases", blurb: "What each release changed." },
];

const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

// A link inside a guide: another guide stays in the app, the rest open as is.
let current = "";
function href(url) {
  if (url.startsWith("#")) return `#docs/${current}${url}`;
  const m = /^(?:\.\/)?([a-z0-9-]+)\.md(#.*)?$/i.exec(url);
  if (m && GUIDES.some((g) => g.id === m[1])) return `#docs/${m[1]}${m[2] || ""}`;
  return url;
}

function inline(text) {
  const code = [];
  let s = text.replace(/`([^`]+)`/g, (_, c) => `\u0000${code.push(c) - 1}\u0000`);
  s = esc(s)
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, t, u) => {
      const h = href(u);
      const ext = /^https?:/.test(h) ? ' target="_blank" rel="noopener"' : "";
      return `<a href="${esc(h)}"${ext}>${t}</a>`;
    })
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[\s(])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
    .replace(/(^|[\s(])_([^_\s][^_]*)_(?=[\s).,;:]|$)/g, "$1<em>$2</em>");
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(code[i])}</code>`);
}

const slug = (t) => t.toLowerCase().replace(/<[^>]+>|`/g, "").replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

// Enough Markdown for our guides: headings, paragraphs, lists, code blocks,
// tables, quotes and inline code, bold, italics and links.
export function markdown(src) {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out = [];
  let i = 0;
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/).map((c) => inline(c.trim().replace(/\\\|/g, "|")));
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    let m;
    if ((m = /^```(\w*)/.exec(line))) {
      const body = [];
      for (i++; i < lines.length && !/^```/.test(lines[i]); i++) body.push(lines[i]);
      i++;
      out.push(`<pre><code${m[1] ? ` class="lang-${esc(m[1])}"` : ""}>${esc(body.join("\n"))}</code></pre>`);
    } else if ((m = /^(#{1,4})\s+(.*)$/.exec(line))) {
      const n = m[1].length, t = inline(m[2]);
      out.push(`<h${n + 1} id="${slug(m[2])}">${t}</h${n + 1}>`);
      i++;
    } else if (/^\|/.test(line) && /^\|?\s*:?-{3,}/.test(lines[i + 1] || "")) {
      const head = cells(line);
      i += 2;
      const rows = [];
      while (i < lines.length && /^\|/.test(lines[i])) rows.push(cells(lines[i++]));
      out.push(
        `<div class="table-wrap"><table><thead><tr>${head.map((c) => `<th>${c}</th>`).join("")}</tr></thead><tbody>` +
          rows.map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join("")}</tr>`).join("") +
          "</tbody></table></div>",
      );
    } else if (/^>\s?/.test(line)) {
      const body = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) body.push(lines[i++].replace(/^>\s?/, ""));
      out.push(`<blockquote>${markdown(body.join("\n"))}</blockquote>`);
    } else if (/^\s*([*-]|\d+\.)\s+/.test(line)) {
      const ordered = /^\s*\d+\./.test(line);
      const items = [];
      while (i < lines.length && lines[i].trim() && (/^\s*([*-]|\d+\.)\s+/.test(lines[i]) || /^\s{2,}\S/.test(lines[i]))) {
        if (/^\s*([*-]|\d+\.)\s+/.test(lines[i]) && !/^\s{2,}([*-]|\d+\.)\s/.test(lines[i])) {
          items.push(lines[i].replace(/^\s*([*-]|\d+\.)\s+/, ""));
        } else {
          items[items.length - 1] += "\n" + lines[i];
        }
        i++;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(
        `<${tag}>${items
          .map((it) => {
            const [head, ...more] = it.split("\n");
            // Wrapped lines belong to the item's text, up to a nested block.
            let first = head, k = 0;
            while (k < more.length && !/^\s*([*-]|\d+\.)\s+|^\s*```/.test(more[k])) first += " " + more[k++].trim();
            const rest = more.slice(k);
            const nested = rest.length ? markdown(rest.map((l) => l.replace(/^\s{2,4}/, "")).join("\n")) : "";
            return `<li>${inline(first)}${nested}</li>`;
          })
          .join("")}</${tag}>`,
      );
    } else {
      const para = [];
      while (
        i < lines.length && lines[i].trim() &&
        !/^(```|#{1,4}\s|>|\s*([*-]|\d+\.)\s+)/.test(lines[i]) &&
        !(/^\|/.test(lines[i]) && /^\|?\s*:?-{3,}/.test(lines[i + 1] || ""))
      ) para.push(lines[i++].trim());
      out.push(`<p>${inline(para.join(" "))}</p>`);
    }
  }
  return out.join("\n");
}

const view = () => document.getElementById("docsView");
const play = () => document.querySelector("main.play");
const cache = new Map();

async function show(id, anchor) {
  const v = view();
  const list = GUIDES.map(
    (g) => `<li><a href="#docs/${g.id}"${g.id === id ? ' aria-current="page"' : ""}>${esc(g.title)}</a></li>`,
  ).join("");
  const nav = `<nav class="docs-nav" aria-label="Guides"><a class="back" href="#">← Back to the board</a><ul>${list}</ul></nav>`;
  if (!id) {
    v.innerHTML = `${nav}<article class="docs-body"><h1>Guides</h1><ul class="docs-cards">${GUIDES.map(
      (g) => `<li><a href="#docs/${g.id}"><strong>${esc(g.title)}</strong><span>${esc(g.blurb)}</span></a></li>`,
    ).join("")}</ul></article>`;
    document.title = "Guides · Chess lab";
    return;
  }
  const g = GUIDES.find((x) => x.id === id);
  if (!g) { location.hash = "#docs"; return; }
  v.innerHTML = `${nav}<article class="docs-body"><p class="hint">Loading ${esc(g.title)}…</p></article>`;
  document.title = `${g.title} · Chess lab`;
  try {
    current = id;
    if (!cache.has(id)) {
      const r = await fetch(`docs/${id}.md`);
      if (!r.ok) throw new Error(`${r.status}`);
      cache.set(id, markdown(await r.text()));
    }
    v.querySelector(".docs-body").innerHTML = cache.get(id);
    const target = anchor && document.getElementById(anchor);
    if (target) target.scrollIntoView({ block: "start" });
    else scrollTo(0, 0);
  } catch (e) {
    v.querySelector(".docs-body").innerHTML =
      `<h1>${esc(g.title)}</h1><p>This guide isn't available here (${esc(String(e.message || e))}). ` +
      `It's in the repository as <a href="https://github.com/OffeneDatenmodellierung/open-chess/blob/main/docs/${id}.md" target="_blank" rel="noopener">docs/${id}.md</a>.</p>`;
  }
}

function route() {
  const m = /^#docs(?:\/([a-z0-9-]+))?(?:#(.+))?$/i.exec(location.hash);
  const open = !!m;
  view().hidden = !open;
  play().hidden = open;
  document.documentElement.classList.toggle("reading", open);
  document.getElementById("openDocs")?.setAttribute("aria-pressed", String(open));
  if (open) show(m[1] || "", m[2]);
  else document.title = "Chess lab";
}

export function startDocs() {
  addEventListener("hashchange", route);
  // Choosing what to play goes back to the board.
  document.querySelectorAll("[data-mode]").forEach((b) =>
    b.addEventListener("click", () => { if (location.hash.startsWith("#docs")) location.hash = ""; }),
  );
  route();
}
