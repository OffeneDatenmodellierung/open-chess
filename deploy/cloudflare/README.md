# The chess server on Cloudflare

This deploys `open-chess` (see `crates/chess-server/README.md`) as a Worker
in front of Cloudflare Containers. The API runs on `CHESS_INSTANCES`
containers, picked at random per request, and one fair-play worker
container is woken by a cron every five minutes. Both containers use the
image from `Dockerfile`, with different entrypoints. They use the
site's database; nothing is kept in the containers.

```
player / bot ──▶ chess.example.com ──▶ chess Worker ──▶ Chess container × CHESS_INSTANCES ──▶ Postgres
                                                    └── cron ──▶ Analyse container (Stockfish) ─────────┘
```

## Configure

1. In `wrangler.jsonc`, set the `routes` host (the chess server's own
   host, e.g. `chess.example.com`; its zone must be in this Cloudflare
   account), `CHESS_PUBLIC_URL` to match, `ESPORTS_PUBLIC_URL`,
   `ESPORTS_LOGIN` (`"true"` to let people sign in to apps through that
   site), `CHESS_SITE_NAME` and `CHESS_INSTANCES`.
2. Set the secrets:

   ```sh
   npm install
   npx wrangler secret put DATABASE_URL     # the site's Postgres URL (pooled is fine)
   # optional: CHESS_LISTEN_DATABASE_URL if the direct host can't be derived,
   #           CHESS_* settings from crates/chess-server/README.md
   ```

3. Deploy:
   * `npx wrangler deploy` with Docker running (from this directory).
4. Point your site at it (its Lichess backend's base URL becomes
   `https://chess.example.com`), and set the site's session cookie domain to
   the shared parent (`example.com`) so the consent page sees site sign-ins.
   The fair-play screening model is a secret too:
   `npx wrangler secret put CHESS_FAIRPLAY_MODEL` (the JSON itself).

   The cookie domain only matters for the OAuth consent page (third-party
   apps and bots asking for a token). Games played on the site's board
   use tokens the site mints, which work without it.

   To serve it under the site at `/chess` instead: drop `routes`, set
   `CHESS_PUBLIC_URL` to `https://<site>/chess`, deploy, then uncomment the
   service binding in your site's Worker and deploy it again.

5. **The training app** is part of the image and goes live with the
   deploy at `https://chess.example.com/app/` (browsers opening the
   bare host are sent there). Nothing to configure; it installs to a
   phone's home screen from that page and works offline after the first
   visit.

6. **Photo reading** (optional): give the containers a vision model with an
   OpenAI-style `/chat/completions` API:

   ```sh
   npx wrangler secret put CHESS_VISION_ENDPOINT   # e.g. https://api.anthropic.com/v1
   npx wrangler secret put CHESS_VISION_MODEL      # the model name that server knows
   npx wrangler secret put CHESS_VISION_API_KEY    # if it needs one
   ```

   (Vars in `wrangler.jsonc` work too for the endpoint and model; keep the
   key a secret.) Then redeploy so new containers pick them up. Without
   them the app's photo buttons explain that photo reading isn't set up.

`npm run check` type-checks the Worker and validates the config.

## Notes

* `https://chess.example.com/` answers with a short JSON description of
  the server (browsers go to the app at `/app/`); the API is under `/api`,
  as on Lichess, with `/oauth` and `/game/export/{id}` beside it
  ([docs/api.md](../../docs/api.md)).
* The image is built from the repository's `Dockerfile`, which compiles
  nothing: it downloads the GitHub release for the checkout's version
  (`Cargo.toml`), so deploy a commit that has a release (a `v*` tag).
* The API containers sleep after 30 minutes without requests, and the next
  request wakes one in a few seconds. Open game streams count as requests.
* The fair-play worker screens the queue and sleeps 20 minutes after the
  cron last woke it. Set `ANALYSE_ENABLED` to `false` to run screening
  elsewhere (Kubernetes, or `open-chess analyse` on any machine with the
  database URL).
* Metrics: there's no Prometheus scrape here. The containers serve
  `/metrics` on 9465 for anything that can reach them, and the Worker's
  logs are in the dashboard.
