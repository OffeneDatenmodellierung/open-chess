// The Worker in front of the open-chess server containers.
//
// Requests go to one of CHESS_INSTANCES containers, picked at random: the
// server keeps nothing in memory that matters (games, clocks and challenges
// are in Postgres), and every instance hears the others' changes through
// Postgres notifications, so a move sent to one reaches a game stream held
// by another. NDJSON streams pass straight through.
//
// A cron wakes the single fair-play worker container, which screens
// finished games with Stockfish and goes back to sleep when idle.

import { Container, getContainer, getRandom } from "@cloudflare/containers";
import { env as workerEnv } from "cloudflare:workers";

interface Env {
  CHESS: DurableObjectNamespace<Chess>;
  ANALYSE: DurableObjectNamespace<Analyse>;
  CHESS_INSTANCES?: string;
  CHESS_DEPLOY_ID?: string;
  ANALYSE_ENABLED?: string;
  [key: string]: unknown;
}

// Worker vars and secrets handed to the containers (only those that are set).
const PASS_THROUGH = [
  "DATABASE_URL",
  "CHESS_LISTEN_DATABASE_URL",
  "CHESS_PUBLIC_URL",
  "ESPORTS_PUBLIC_URL",
  "ESPORTS_LOGIN",
  "ESPORTS_SECRET",
  "CHESS_SIGNUP",
  // The Chess lab's look: a bundled theme's name (a var, e.g. esports-lab),
  // or your own CSS as a secret.
  "CHESS_APP_THEME",
  "CHESS_SITE_NAME",
  // The host site's name on "Sign in with …".
  "CHESS_HOST_NAME",
  "CHESS_SITE_SCHEMA",
  "CHESS_SCHEMA",
  "CHESS_POOL_SIZE",
  "CHESS_LAG_COMP_MS",
  "CHESS_CHALLENGE_TTL_SECS",
  "CHESS_FIRST_MOVE_SECS",
  "CHESS_GONE_AFTER_SECS",
  "CHESS_CLAIM_WIN_SECS",
  "CHESS_PUBLIC_DELAY_PLIES",
  "CHESS_FAIRPLAY",
  "CHESS_BLOCK_MARKED",
  "CHESS_ANALYSIS_DEPTH",
  // The screening model (keep it a secret: `wrangler secret put CHESS_FAIRPLAY_MODEL`).
  "CHESS_FAIRPLAY_MODEL",
  "CHESS_FAIRPLAY_FLAG_SCORE",
  "CHESS_FAIRPLAY_MIN_MOVES",
  "CHESS_FAIRPLAY_APP_DEPTH",
  "CHESS_FAIRPLAY_APP_NODES",
  "CHESS_FAIRPLAY_APP_NET",
  // Photo reading for the training app (the key is a secret).
  "CHESS_VISION_ENDPOINT",
  "CHESS_VISION_MODEL",
  "CHESS_VISION_API_KEY",
  "CHESS_VISION_LOCAL_MODEL",
  "CHESS_VISION_MIN_CONFIDENCE",
  "CHESS_VISION_REVIEW",
  "CHESS_VISION_REVIEW_MAX",
  "CHESS_APP_DIR",
  // Admins by email, and how long an admin's passkey sign-in lasts.
  "CHESS_ADMIN_EMAILS",
  "CHESS_ADMIN_PASSKEY_HOURS",
  "CHESS_FAIRPLAY_SKIP_PLIES",
  "CHESS_TABLEBASE_URL",
  "CHESS_ANALYSIS_THREADS",
  "CHESS_ANALYSIS_HASH_MB",
  "CHESS_SWEEP_MS",
  // The iOS app's push notifications (the .p8 key is a secret).
  "CHESS_APNS_KEY",
  "CHESS_APNS_KEY_ID",
  "CHESS_APNS_TEAM_ID",
  "CHESS_APNS_TOPIC",
  // Sign in with Apple, in the app and on the sign-in page (the key is a
  // secret; the ids are vars).
  "CHESS_APPLE_KEY",
  "CHESS_APPLE_KEY_ID",
  "CHESS_APPLE_TEAM_ID",
  "CHESS_APPLE_CLIENT_IDS",
  "CHESS_APPLE_SERVICES_ID",
  "CHESS_APPLE_URL",
  "RUST_LOG",
];

function appEnv(e: Record<string, unknown>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const k of PASS_THROUGH) {
    const v = e[k];
    if (typeof v === "string" && v !== "") out[k] = v;
  }
  return out;
}

export class Chess extends Container<Env> {
  defaultPort = 8090;
  // Asleep after half an hour without requests; the next request wakes it.
  sleepAfter = "30m";
  entrypoint = ["/usr/local/bin/open-chess", "serve"];
  envVars = appEnv(workerEnv as unknown as Record<string, unknown>);
}

export class Analyse extends Container<Env> {
  // Its health endpoint: the cron's request is what wakes it.
  defaultPort = 9465;
  sleepAfter = "20m";
  entrypoint = ["/usr/local/bin/open-chess", "analyse"];
  envVars = appEnv(workerEnv as unknown as Record<string, unknown>);
}

// Without the database the server can't start (it exits at once, code 2,
// and the container restarts in a loop): say so instead. Secrets belong to
// each Worker; this one doesn't see the site's.
const NO_DATABASE =
  "DATABASE_URL isn't set on this Worker (open-chess). Set it to the site's Postgres URL: " +
  "npx wrangler secret put DATABASE_URL (in deploy/cloudflare).";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (typeof env.DATABASE_URL !== "string" || env.DATABASE_URL === "") {
      console.error(NO_DATABASE);
      return Response.json({ error: NO_DATABASE }, { status: 503 });
    }
    const instances = Math.max(1, Number.parseInt(env.CHESS_INSTANCES ?? "2", 10) || 1);
    const headers = new Headers(request.headers);
    const ip = request.headers.get("cf-connecting-ip");
    if (ip) headers.set("x-forwarded-for", ip);
    // A container keeps the settings it started with, so each deploy has
    // its own instances (CHESS_DEPLOY_ID, set by the deploy): they start
    // with this deploy's settings, and the last deploy's go to sleep.
    const deploy = env.CHESS_DEPLOY_ID ?? "";
    const n = Math.floor(Math.random() * instances);
    const container = deploy
      ? getContainer(env.CHESS, `${deploy}-${n}`)
      : await getRandom(env.CHESS, instances);
    return container.fetch(new Request(request, { headers }));
  },

  async scheduled(_event: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    if (env.ANALYSE_ENABLED === "false") return;
    if (typeof env.DATABASE_URL !== "string" || env.DATABASE_URL === "") {
      console.error(NO_DATABASE);
      return;
    }
    const worker = getContainer(env.ANALYSE, env.CHESS_DEPLOY_ID ? `fairplay-${env.CHESS_DEPLOY_ID}` : "fairplay");
    ctx.waitUntil(worker.fetch(new Request("http://analyse/healthz")).then(() => undefined));
  },
};
