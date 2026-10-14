// Writes wrangler.ci.json for the deploy workflow: wrangler.jsonc with this
// deployment's settings (repository variables) and the pushed image.
//
//   IMAGE               registry.cloudflare.com/… (both container classes)
//   CHESS_WORKER_NAME   the Worker's name (keep it once deployed: a new
//                       name is a new Worker)
//   CHESS_PUBLIC_URL    where players reach it; its host is the route
//   CHESS_WORKER_VARS   JSON object of more vars (ESPORTS_PUBLIC_URL, …)
//
// Secrets (DATABASE_URL, ESPORTS_SECRET, CHESS_FAIRPLAY_MODEL,
// CHESS_APP_THEME, …) stay in Cloudflare: `wrangler secret put`.
import { readFileSync, writeFileSync } from "node:fs";

/** JSONC to JSON: drops comments outside strings, then trailing commas. */
function parseJsonc(text) {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === "\\" ? 2 : 1;
      out += text.slice(i, j + 1);
      i = j;
    } else if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
      out += "\n";
    } else if (c === "/" && text[i + 1] === "*") {
      i = text.indexOf("*/", i + 2) + 1;
    } else {
      out += c;
    }
  }
  return JSON.parse(out.replace(/,(\s*[}\]])/g, "$1"));
}

const env = process.env;
const cfg = parseJsonc(readFileSync("wrangler.jsonc", "utf8"));

if (!env.IMAGE) throw new Error("IMAGE isn't set");
for (const c of cfg.containers) {
  c.image = env.IMAGE;
  delete c.image_build_context;
}
if (env.CHESS_WORKER_NAME) cfg.name = env.CHESS_WORKER_NAME;
if (env.CHESS_PUBLIC_URL) {
  const url = new URL(env.CHESS_PUBLIC_URL);
  cfg.vars.CHESS_PUBLIC_URL = env.CHESS_PUBLIC_URL.replace(/\/$/, "");
  // Its own host: the route. Under a path (/chess), the site's Worker
  // forwards to this one, so no route.
  if (url.pathname === "/" || url.pathname === "") {
    cfg.routes = [{ pattern: url.host, custom_domain: true }];
  } else {
    delete cfg.routes;
  }
}
if (env.CHESS_WORKER_VARS) {
  const more = JSON.parse(env.CHESS_WORKER_VARS);
  for (const [k, v] of Object.entries(more)) cfg.vars[k] = String(v);
}
if (cfg.routes?.some((r) => r.pattern.endsWith("example.com"))) {
  throw new Error("Set the CHESS_PUBLIC_URL repository variable (the route is still chess.example.com).");
}

writeFileSync("wrangler.ci.json", JSON.stringify(cfg, null, 2) + "\n");
console.log(`${cfg.name}: ${cfg.vars.CHESS_PUBLIC_URL}, image ${env.IMAGE}`);
console.log("vars:", Object.keys(cfg.vars).join(", "));
