#!/usr/bin/env python3
"""Talk to the App Store Connect API with the team key, for the TestFlight workflow.

  app-store-connect.py check            does App Store Connect take the key, and has it the app?
  app-store-connect.py upload APP.ipa   upload a build (Apple's build upload API) and wait for it

Reads KEY_ID, ISSUER, KEY (the .p8 text or its base64) and BUNDLE_ID from the
environment. Problems are printed as GitHub annotations; nothing secret is.
Needs pyjwt[crypto].

The upload goes through the REST API rather than xcodebuild's or altool's
uploader: those sign in through another Apple service that refused this key
("Cannot verify client's JWT headers") while the API itself takes it.
"""

import base64
import hashlib
import json
import os
import plistlib
import re
import sys
import time
import urllib.error
import urllib.request
import zipfile

import jwt
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import load_pem_private_key

API = "https://api.appstoreconnect.apple.com"
problems = []


def note(msg):
    print(f"::notice::{msg}", flush=True)


def warn(msg):
    print(f"::warning::{msg}", flush=True)


def err(msg):
    problems.append(msg)
    print(f"::error::{msg}", flush=True)


def fail(msg):
    err(msg)
    sys.exit(1)


def load_key():
    raw_id, raw_issuer, raw_key = os.environ["KEY_ID"], os.environ["ISSUER"], os.environ["KEY"]
    key_id, issuer = raw_id.strip(), raw_issuer.strip()
    if raw_id != key_id:
        warn("APP_STORE_CONNECT_KEY_ID has spaces or a newline around it (trimmed)")
    if raw_issuer != issuer:
        warn("APP_STORE_CONNECT_ISSUER_ID has spaces or a newline around it (trimmed)")
    if not re.fullmatch(r"[A-Z0-9]{10}", key_id):
        err(f"APP_STORE_CONNECT_KEY_ID should be 10 capital letters and digits; it is {len(key_id)} characters")
    if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", issuer):
        err("APP_STORE_CONNECT_ISSUER_ID should look like 57246542-96fe-1a63-e053-0824d011072a (lower case, with dashes)")
    if "BEGIN PRIVATE KEY" in raw_key:
        pem, form = raw_key.strip().replace("\\n", "\n").encode(), "PEM text"
    else:
        try:
            pem, form = base64.b64decode(raw_key.strip()), "base64"
        except Exception:
            fail("APP_STORE_CONNECT_KEY is neither the .p8 text nor base64 of it")
    try:
        key = load_pem_private_key(pem, None)
    except Exception as e:
        fail(f"APP_STORE_CONNECT_KEY ({form}) isn't a readable private key: {e}")
    if not (isinstance(key, ec.EllipticCurvePrivateKey) and key.curve.name == "secp256r1"):
        fail("APP_STORE_CONNECT_KEY isn't an App Store Connect key (P-256)")
    return key_id, issuer, key, form


class Api:
    def __init__(self):
        self.key_id, self.issuer, self.key, self.form = load_key()
        self._token, self._made = None, 0

    def token(self):
        now = int(time.time())
        if not self._token or now - self._made > 600:  # tokens last 20 minutes at most
            self._token = jwt.encode(
                {"iss": self.issuer, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"},
                self.key, algorithm="ES256", headers={"kid": self.key_id, "typ": "JWT"})
            self._made = now
        return self._token

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(API + path, data=data, method=method, headers={
            "Authorization": f"Bearer {self.token()}", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                text = r.read()
                return json.loads(text) if text else {}
        except urllib.error.HTTPError as e:
            raw = e.read().decode(errors="replace")
            try:
                details = "; ".join(f"{x.get('title')}: {x.get('detail')}" for x in json.loads(raw)["errors"])
            except Exception:
                details = raw[:500]
            hint = {
                401: "the key id, issuer id and .p8 don't belong together, or the key was revoked, or it is an Individual key (needs a Team key)",
                403: "the key's role can't do this (use Admin or App Manager), or an agreement is waiting in App Store Connect › Business",
                409: "App Store Connect already has this version and build number, or the request didn't fit",
            }.get(e.code, "")
            fail(f"{method} {path.split('?')[0]}: App Store Connect answered {e.code}: {details}" + (f" (likely {hint})" if hint else ""))
        except urllib.error.URLError as e:
            fail(f"couldn't reach App Store Connect: {e.reason}")

    def app(self, bundle_id):
        apps = self.call("GET", f"/v1/apps?filter[bundleId]={bundle_id}&fields[apps]=name,bundleId")["data"]
        exact = [a for a in apps if a["attributes"]["bundleId"] == bundle_id]
        if not exact:
            fail(f"App Store Connect takes the key but has no app with bundle id {bundle_id}: add it under Apps › +")
        return exact[0]


def check():
    api = Api()
    note(f"APP_STORE_CONNECT_KEY: a P-256 key, given as {api.form}")
    app = api.app(os.environ["BUNDLE_ID"])
    note(f"App Store Connect takes the key, and has the app: {app['attributes']['name']}")


def ipa_versions(path):
    with zipfile.ZipFile(path) as z:
        names = [n for n in z.namelist() if re.fullmatch(r"Payload/[^/]+\.app/Info\.plist", n)]
        if not names:
            fail(f"{path} has no Payload/*.app/Info.plist")
        info = plistlib.loads(z.read(names[0]))
    return info["CFBundleShortVersionString"], info["CFBundleVersion"], info["CFBundleIdentifier"]


def upload(path):
    api = Api()
    version, build, bundle_id = ipa_versions(path)
    if bundle_id != os.environ["BUNDLE_ID"]:
        fail(f"the .ipa is {bundle_id}, expected {os.environ['BUNDLE_ID']}")
    app = api.app(bundle_id)
    size = os.path.getsize(path)
    note(f"uploading {app['attributes']['name']} {version} ({build}), {size / 1e6:.1f} MB")

    bu = api.call("POST", "/v1/buildUploads", {"data": {
        "type": "buildUploads",
        "attributes": {"cfBundleShortVersionString": version, "cfBundleVersion": build, "platform": "IOS"},
        "relationships": {"app": {"data": {"type": "apps", "id": app["id"]}}}}})["data"]
    f = api.call("POST", "/v1/buildUploadFiles", {"data": {
        "type": "buildUploadFiles",
        "attributes": {"assetType": "ASSET", "fileName": os.path.basename(path), "fileSize": size, "uti": "com.apple.ipa"},
        "relationships": {"buildUpload": {"data": {"type": "buildUploads", "id": bu["id"]}}}}})["data"]

    ops = f["attributes"].get("uploadOperations") or []
    if not ops:
        fail("App Store Connect gave no upload operations for the file")
    with open(path, "rb") as fh:
        for i, op in enumerate(ops, 1):
            fh.seek(op["offset"])
            chunk = fh.read(op["length"])
            headers = {h["name"]: h["value"] for h in op.get("requestHeaders") or []}
            for attempt in range(1, 4):
                req = urllib.request.Request(op["url"], data=chunk, method=op["method"], headers=headers)
                try:
                    with urllib.request.urlopen(req, timeout=300) as r:
                        r.read()
                    break
                except (urllib.error.HTTPError, urllib.error.URLError) as e:
                    if attempt == 3:
                        fail(f"uploading part {i} of {len(ops)} failed: {e}")
                    time.sleep(5 * attempt)
    print(f"uploaded {len(ops)} part(s)", flush=True)

    md5 = hashlib.md5()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            md5.update(block)
    api.call("PATCH", f"/v1/buildUploadFiles/{f['id']}", {"data": {
        "type": "buildUploadFiles", "id": f["id"],
        "attributes": {"uploaded": True, "sourceFileChecksums": {"file": {"hash": md5.hexdigest(), "algorithm": "MD5"}}}}})

    # Apple checks the build; its errors (a missing icon, a bad entitlement)
    # come back here, as they would in the email Apple sends otherwise.
    deadline = time.time() + 30 * 60
    while True:
        state = api.call("GET", f"/v1/buildUploads/{bu['id']}")["data"]["attributes"].get("state") or {}
        for w in state.get("warnings") or []:
            warn(f"App Store Connect: {w.get('code')}: {w.get('description')}")
        s = state.get("state")
        if s == "COMPLETE":
            note(f"{version} ({build}) is in App Store Connect: it shows in TestFlight once Apple has processed it")
            return
        if s == "FAILED":
            for e in state.get("errors") or []:
                err(f"App Store Connect: {e.get('code')}: {e.get('description')}")
            fail(f"App Store Connect turned down {version} ({build})")
        if time.time() > deadline:
            warn(f"{version} ({build}) is uploaded and still {s} after 30 minutes: watch App Store Connect › TestFlight")
            return
        print(f"App Store Connect: {s}", flush=True)
        time.sleep(30)


if __name__ == "__main__":
    if sys.argv[1:] == ["check"]:
        check()
    elif len(sys.argv) == 3 and sys.argv[1] == "upload":
        upload(sys.argv[2])
    else:
        sys.exit(__doc__)
    sys.exit(1 if problems else 0)
