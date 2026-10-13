#!/usr/bin/env python3
"""Publish the workspace's crates to crates.io, dependencies first.

Skips versions already published (so a failed release can be re-run) and
waits out crates.io's rate limit on new crates (a few at once, then one
every ten minutes). Needs CARGO_REGISTRY_TOKEN. `--dry-run` lists what it
would publish.
"""

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

DRY = "--dry-run" in sys.argv


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def index_path(name):
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def published(name, version):
    url = f"https://index.crates.io/{index_path(name)}"
    try:
        with urllib.request.urlopen(url, timeout=30) as r:
            lines = r.read().decode().splitlines()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise
    return any(json.loads(line).get("vers") == version for line in lines if line.strip())


def order(packages):
    """Workspace packages that publish, each after its workspace dependencies."""
    names = {p["name"]: p for p in packages if p.get("publish") != []}
    done, out = set(), []

    def visit(name, stack=()):
        if name in done:
            return
        if name in stack:
            sys.exit(f"dependency cycle through {name}")
        for d in names[name]["dependencies"]:
            # Dev-dependencies too: crates.io needs them published first.
            if d["name"] in names and d["name"] != name:
                visit(d["name"], stack + (name,))
        done.add(name)
        out.append(names[name])

    for n in sorted(names):
        visit(n)
    return out


def main():
    for p in order(metadata()["packages"]):
        name, version = p["name"], p["version"]
        if published(name, version):
            print(f"{name} {version}: already on crates.io")
            continue
        if DRY:
            print(f"{name} {version}: would publish")
            continue
        while True:
            print(f"{name} {version}: publishing", flush=True)
            r = subprocess.run(["cargo", "publish", "--locked", "-p", name], capture_output=True, text=True)
            sys.stdout.write(r.stdout)
            sys.stderr.write(r.stderr)
            if r.returncode == 0:
                break
            if "429" in r.stderr or "Too Many Requests" in r.stderr or "rate limit" in r.stderr.lower():
                print("crates.io's rate limit: waiting ten minutes", flush=True)
                time.sleep(600)
                continue
            if published(name, version):
                break
            sys.exit(f"{name}: publishing failed")


if __name__ == "__main__":
    main()
