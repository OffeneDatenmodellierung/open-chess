#!/usr/bin/env python3
"""Check the chess server's JSON against Lichess's own OpenAPI spec.

Drives a running open-chess through a full Board API game, challenges
(accepted, declined, canceled), the event, game and move streams, exports,
token test and bot listing, and validates every response and stream line
against the schemas in github.com/lichess-org/api (doc/specs).

  git clone --depth 1 https://github.com/lichess-org/api /tmp/lichess-api
  pip install jsonschema referencing pyyaml requests
  scripts/chess-conformance.py --base http://127.0.0.1:8090 \\
      --spec /tmp/lichess-api/doc/specs \\
      --db postgres://dev:dev@127.0.0.1/chessdev --site-schema esports

It makes three throwaway players (with tokens) in the site schema through
psql, and prints one line per check; exit status 1 if any fail.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import queue
import secrets
import subprocess
import sys
import threading
import time

import requests
import yaml
from jsonschema import Draft202012Validator
from referencing import Registry, Resource
from referencing.jsonschema import DRAFT202012

FAILS: list[str] = []


def load_yaml(path: pathlib.Path):
    return yaml.safe_load(path.read_text())


class Spec:
    def __init__(self, root: pathlib.Path):
        self.root = root.resolve()

        def retrieve(uri: str):
            p = pathlib.Path(uri.removeprefix("file://"))
            return Resource.from_contents(load_yaml(p), default_specification=DRAFT202012)

        self.registry = Registry(retrieve=retrieve)

    def schema(self, name: str) -> tuple[dict, str]:
        p = self.root / "schemas" / f"{name}.yaml"
        return load_yaml(p), p.as_uri()

    def response(self, tag_file: str, method: str = "get", status: str = "200", ctype: str = "application/json"):
        p = self.root / "tags" / tag_file
        op = load_yaml(p)[method]
        return op["responses"][status]["content"][ctype]["schema"], p.as_uri()

    def check(self, label: str, instance, schema_and_base) -> None:
        schema, base = schema_and_base
        wrapped = {"$id": base, **schema} if isinstance(schema, dict) else schema
        v = Draft202012Validator(wrapped, registry=self.registry)
        errors = sorted(v.iter_errors(instance), key=lambda e: list(e.path))
        if errors:
            FAILS.append(label)
            print(f"FAIL {label}")
            for e in errors[:5]:
                print(f"     at {'/'.join(map(str, e.path)) or '.'}: {e.message[:200]}")
        else:
            print(f"ok   {label}")


def psql(db: str, sql: str) -> str:
    return subprocess.run(["psql", db, "-tAc", sql], check=True, capture_output=True, text=True).stdout.strip()


def make_player(db: str, schema: str, name: str, scopes: list[str]) -> str:
    pid = psql(db, f"INSERT INTO {schema}.players (name) VALUES ('{name}') RETURNING id").splitlines()[0]
    token = "esl_" + secrets.token_hex(18)
    h = hashlib.sha256(token.encode()).hexdigest()
    sc = "{" + ",".join(scopes) + "}"
    psql(db, f"INSERT INTO {schema}.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at) "
             f"VALUES ({pid}, 'conformance', '{h}', 'esl_xx', '{sc}', now() + interval '1 hour')")
    return token


class Stream(threading.Thread):
    """Read an NDJSON stream into a queue, skipping keep-alives."""

    def __init__(self, url: str, token: str | None):
        super().__init__(daemon=True)
        self.url, self.token, self.q = url, token, queue.Queue()

    def run(self):
        h = {"Authorization": f"Bearer {self.token}"} if self.token else {}
        with requests.get(self.url, headers=h, stream=True, timeout=60) as r:
            for line in r.iter_lines():
                if line and line.strip():
                    self.q.put(json.loads(line))

    def next(self, want=None, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            try:
                v = self.q.get(timeout=max(0.05, end - time.time()))
            except queue.Empty:
                break
            if want is None or v.get("type") == want:
                return v
        raise TimeoutError(f"no {want or 'line'} on {self.url}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--spec", required=True)
    ap.add_argument("--db", required=True)
    ap.add_argument("--site-schema", default="esports")
    a = ap.parse_args()
    spec = Spec(pathlib.Path(a.spec))
    base = a.base.rstrip("/")
    tag = secrets.token_hex(3)
    human = ["board:play", "challenge:read", "challenge:write", "email:read"]
    ta = make_player(a.db, a.site_schema, f"Confa{tag}", human)
    tb = make_player(a.db, a.site_schema, f"Confb{tag}", human)
    tbot = make_player(a.db, a.site_schema, f"Confbot{tag}", ["bot:play", "challenge:read", "challenge:write"])

    def api(method: str, path: str, token: str | None, **kw):
        h = kw.pop("headers", {})
        if token:
            h["Authorization"] = f"Bearer {token}"
        return requests.request(method, base + path, headers=h, timeout=20, **kw)

    me = api("GET", "/api/account", ta).json()
    spec.check("GET /api/account", me, spec.schema("UserExtended"))
    ua = me["id"]
    ub = api("GET", "/api/account", tb).json()["id"]
    bot = api("GET", "/api/account", tbot).json()["id"]
    api("POST", "/api/bot/account/upgrade", tbot).raise_for_status()
    spec.check("GET /api/user/{username}", api("GET", f"/api/user/{ua}", None).json(), spec.schema("UserExtended"))

    ev_a = Stream(base + "/api/stream/event", ta)
    ev_b = Stream(base + "/api/stream/event", tb)
    ev_a.start()
    ev_b.start()
    time.sleep(0.5)
    event_schema = spec.response("board/api-stream-event.yaml", ctype="application/x-ndjson")

    # A challenge, listed, received and accepted.
    ch = api("POST", f"/api/challenge/{ub}", ta, data={"clock.limit": 180, "clock.increment": 2, "color": "white"}).json()
    spec.check("POST /api/challenge/{username}", ch, spec.schema("ChallengeJson"))
    lst = api("GET", "/api/challenge", tb).json()
    spec.check("GET /api/challenge", lst, spec.response("challenges/api-challenge.yaml"))
    spec.check("event stream: challenge", ev_b.next("challenge"), event_schema)
    shown = api("GET", f"/api/challenge/{ch['id']}/show", tb).json()
    spec.check("GET /api/challenge/{id}/show", shown, spec.schema("ChallengeJson"))
    ok = api("POST", f"/api/challenge/{ch['id']}/accept", tb).json()
    spec.check("POST /api/challenge/{id}/accept", ok, spec.schema("Ok"))
    spec.check("event stream: gameStart", ev_a.next("gameStart"), event_schema)
    gid = ch["id"]

    board_schema = spec.response("board/api-board-game-stream-gameId.yaml", ctype="application/x-ndjson")
    gs = Stream(base + f"/api/board/game/stream/{gid}", ta)
    gs.start()
    spec.check("board stream: gameFull", gs.next("gameFull"), board_schema)
    api("POST", f"/api/board/game/{gid}/move/e2e4", ta).raise_for_status()
    spec.check("board stream: gameState", gs.next("gameState"), board_schema)
    err = api("POST", f"/api/board/game/{gid}/move/e2e4", ta)
    spec.check(f"move out of turn ({err.status_code})", err.json(), spec.schema("Error"))
    api("POST", f"/api/board/game/{gid}/chat", tb, data={"room": "player", "text": "good luck"}).raise_for_status()
    spec.check("board stream: chatLine", gs.next("chatLine"), board_schema)
    api("POST", f"/api/board/game/{gid}/move/e7e5", tb).raise_for_status()
    gs.next("gameState")
    playing = api("GET", "/api/account/playing", ta).json()
    spec.check("GET /api/account/playing", playing, spec.response("games/api-account-playing.yaml"))
    chat = api("GET", f"/api/board/game/{gid}/chat", ta).json()
    spec.check("GET /api/board/game/{id}/chat", chat, spec.response("board/api-board-game-gameId-chat.yaml"))

    ms = Stream(base + f"/api/stream/game/{gid}", None)
    ms.start()
    move_schema = spec.schema("MoveStreamEntry")
    spec.check("move stream: description", ms.next(), move_schema)
    spec.check("move stream: position", ms.next(), move_schema)

    api("POST", f"/api/board/game/{gid}/resign", tb).raise_for_status()
    spec.check("board stream: final gameState", gs.next("gameState"), board_schema)
    spec.check("event stream: gameFinish", ev_a.next("gameFinish"), event_schema)
    exp = api("GET", f"/game/export/{gid}?pgnInJson=true&clocks=true", None, headers={"Accept": "application/json"}).json()
    spec.check("GET /game/export/{id} (json)", exp, spec.schema("GameJson"))
    pgn = api("GET", f"/game/export/{gid}", None)
    if pgn.text.startswith("[Event "):
        print("ok   GET /game/export/{id} (pgn)")
    else:
        print("FAIL GET /game/export/{id} (pgn)")
        FAILS.append("pgn export")

    # Declined and canceled.
    ch2 = api("POST", f"/api/challenge/{ub}", ta, data={"clock.limit": 60, "clock.increment": 0}).json()
    ev_b.next("challenge")
    api("POST", f"/api/challenge/{ch2['id']}/decline", tb, data={"reason": "later"}).raise_for_status()
    spec.check("event stream: challengeDeclined", ev_a.next("challengeDeclined"), event_schema)
    ch3 = api("POST", f"/api/challenge/{ub}", ta, data={"clock.limit": 60, "clock.increment": 0}).json()
    ev_b.next("challenge")
    api("POST", f"/api/challenge/{ch3['id']}/cancel", ta).raise_for_status()
    spec.check("event stream: challengeCanceled", ev_b.next("challengeCanceled"), event_schema)

    tt = api("POST", "/api/token/test", None, data=f"{ta},esl_nope", headers={"Content-Type": "text/plain"}).json()
    spec.check("POST /api/token/test", tt, spec.response("oauth/api-token-test.yaml", method="post"))
    ev_bot = Stream(base + "/api/stream/event", tbot)
    ev_bot.start()
    time.sleep(0.3)
    online = [json.loads(l) for l in api("GET", "/api/bot/online", None).text.splitlines() if l.strip()]
    for u in online[:1]:
        spec.check("GET /api/bot/online", u, spec.schema("UserExtended"))
    if not any(u["id"] == bot for u in online):
        print(f"FAIL GET /api/bot/online: {bot} not listed")
        FAILS.append("bot online")

    print(f"\n{len(FAILS)} failed" if FAILS else "\nall checks passed")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
