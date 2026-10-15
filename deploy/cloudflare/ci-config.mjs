// Writes wrangler.ci.json for the deploy workflow: wrangler.jsonc with this
// deployment's settings (repository variables) and the pushed image.
//
//   IMAGE               registry.cloudflare.com/… (both container classes)
//   CHESS_WORKER_NAME   the Worker's name (keep it once deployed: a new
//                       name is a new Worker)
//   CHESS_PUBLIC_URL    where players reach it; its host is the route
//   CHESS_WORKER_VARS   JSON object of more vars (ESPORTS_PUBLIC_URL, …)
//   GH_VARS, GH_SECRETS the repository's (and environment's) variables and
//                       secrets, as JSON: those named like a setting the
//                       Worker hands its containers (PASS_THROUGH in
//                       src/index.ts) become the Worker's vars and secrets,
//                       so the server's settings can all live in GitHub.
//
// Secrets go to secrets.json for `wrangler secret bulk`; nothing secret is
// printed. Secrets set in Cloudflare with `wrangler secret put` stay unless
// GitHub has the same name.
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
// The settings the Worker passes to its containers.
const index = readFileSync("src/index.ts", "utf8");
const listed = index.match(/const PASS_THROUGH = \[([\s\S]*?)\];/);
if (!listed) throw new Error("PASS_THROUGH not found in src/index.ts");
const settings = new Set([...listed[1].matchAll(/"([A-Z0-9_]+)"/g)].map((m) => m[1]));
// The Worker's own: how many API containers, and the fair-play worker's cron.
settings.add("CHESS_INSTANCES");
settings.add("ANALYSE_ENABLED");

const fromGithub = (name) => {
  try {
    return JSON.parse(env[name] || "{}") ?? {};
  } catch {
    throw new Error(`${name} isn't JSON`);
  }
};
const ghVars = fromGithub("GH_VARS");
const ghSecrets = fromGithub("GH_SECRETS");
const secrets = {};
// Names set on purpose here (not wrangler.jsonc's placeholders).
const chosen = new Set(env.CHESS_PUBLIC_URL ? ["CHESS_PUBLIC_URL"] : []);
for (const [k, v] of Object.entries(ghVars)) {
  if (settings.has(k) && typeof v === "string" && v !== "") {
    cfg.vars[k] = v;
    chosen.add(k);
  }
}
for (const [k, v] of Object.entries(ghSecrets)) {
  if (settings.has(k) && typeof v === "string" && v !== "") secrets[k] = v;
}

if (env.CHESS_WORKER_VARS) {
  const more = JSON.parse(env.CHESS_WORKER_VARS);
  for (const [k, v] of Object.entries(more)) {
    cfg.vars[k] = String(v);
    chosen.add(k);
  }
}
if (cfg.routes?.some((r) => r.pattern.endsWith("example.com"))) {
  throw new Error("Set the CHESS_PUBLIC_URL repository variable (the route is still chess.example.com).");
}

// A var and a secret can't share a name: a var set on purpose wins; a
// secret beats wrangler.jsonc's placeholder.
for (const k of Object.keys(secrets)) {
  if (chosen.has(k)) delete secrets[k];
  else delete cfg.vars[k];
}

writeFileSync("wrangler.ci.json", JSON.stringify(cfg, null, 2) + "\n");
writeFileSync("secrets.json", JSON.stringify(secrets) + "\n", { mode: 0o600 });
console.log(`${cfg.name}: ${cfg.vars.CHESS_PUBLIC_URL}, image ${env.IMAGE}`);
console.log("vars:", Object.keys(cfg.vars).sort().join(", "));
console.log("secrets from GitHub:", Object.keys(secrets).sort().join(", ") || "none");
