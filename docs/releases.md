# Releases

Every release has prebuilt binaries, the image and the crates, all built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) from one
tag.

## Downloads

On the [releases page](https://github.com/OffeneDatenmodellierung/open-chess/releases),
each checked by `SHA256SUMS`:

| File | For |
|---|---|
| `open-chess-device-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | The board runtime on a Linux PC |
| `open-chess-device-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz` | 64-bit Raspberry Pi OS: the DGT Pi, a Centaur with a Pi Zero 2 W, any Pi 3/4/5 |
| `open-chess-device-vX.Y.Z-arm-unknown-linux-gnueabihf.tar.gz` | ARMv6, 32-bit: the Centaur's original Pi Zero W |
| `open-chess-device-vX.Y.Z-aarch64-apple-darwin.tar.gz` | macOS on Apple silicon |
| `open-chess-device-vX.Y.Z-x86_64-apple-darwin.tar.gz` | macOS on Intel |
| `open-chess-device-vX.Y.Z-x86_64-pc-windows-msvc.zip` | Windows |
| `open-chess-server-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`, `…-aarch64-…` | The server (`open-chess`), Linux |
| `chess-app-vX.Y.Z.tar.gz` | The Chess lab app: static files for any web server, or `--app` / `CHESS_APP_DIR` |

The Linux binaries need glibc 2.35 or newer (Debian 12, Ubuntu 22.04,
Raspberry Pi OS bookworm, or later).

```sh
v=0.1.0 t=aarch64-unknown-linux-gnu
curl -fsSLO https://github.com/OffeneDatenmodellierung/open-chess/releases/download/v$v/open-chess-device-v$v-$t.tar.gz
tar xzf open-chess-device-v$v-$t.tar.gz
sudo install open-chess-device-v$v-$t/open-chess-device /usr/local/bin/
```

**macOS.** Releases are signed with a Developer ID and notarised by Apple
when the project's signing secrets are set (below); then the binary just
runs. An unsigned one downloaded in a browser is quarantined, and macOS
says it "cannot be verified": allow it once with
`xattr -d com.apple.quarantine open-chess-device`, or under System Settings
› Privacy & Security. (Downloads with `curl` aren't quarantined.) The first
Bluetooth scan asks for Bluetooth permission for your terminal app.

**Windows.** The binary isn't code-signed, so SmartScreen may warn ("Windows
protected your PC" › More info › Run anyway).

## The image

```sh
docker pull ghcr.io/offenedatenmodellierung/open-chess:0.1.0        # or :latest
docker pull ghcr.io/offenedatenmodellierung/open-chess:0.1.0-slim   # or :slim
```

For linux/amd64 and linux/arm64. The default image is Debian slim with
Stockfish, for both roles (`serve`, `analyse`); `slim` is distroless with
the API and the app only. Both are made from the release's binaries: the
[Dockerfile](../Dockerfile) compiles nothing. Building it yourself
downloads the release for the checkout's version (`--build-arg VERSION=…`
for another), or takes your own build from `dist/` with
`--build-arg BINARIES=local` (the [README](../README.md#build-and-test)
shows how).

## The crates

The libraries and both programs are on crates.io as `open-chess-*`
(everything but the wasm bindings, the board reader's trainer and the
conformance checker). In your `Cargo.toml`, for example:

```toml
[dependencies]
open-chess-core = "0.1"     # rules, SAN, PGN, clocks
open-chess-client = "0.1"   # a Lichess Board/Bot API client
```

`cargo install open-chess-device` and `cargo install open-chess-server`
build the programs.

## Cutting a release

1. Move the version: `version` under `[workspace.package]` and every
   `version` under `[workspace.dependencies]` in the root `Cargo.toml`, then
   `cargo check` to update `Cargo.lock`. Merge that.
2. Tag main and push the tag:
   ```sh
   git tag v0.2.0 && git push origin v0.2.0
   ```
3. The workflow checks the tag matches the version, builds every binary and
   the app, creates the GitHub release with them and `SHA256SUMS`, pushes
   the images to ghcr.io, and publishes the crates.

A failed run can be re-run: the crates step skips versions already
published, and crates.io's limit on new crates (a few at once, then one
every ten minutes) is waited out, so the first release takes over an hour.

A pull request that changes the workflow, the Dockerfile or the publish
script runs the whole build without releasing anything.

### Secrets

Set under the repository's Settings › Secrets and variables › Actions.
Without them the release still happens: unsigned macOS binaries, and no
crates.

| Secret | What |
|---|---|
| `CARGO_REGISTRY_TOKEN` | A crates.io API token with the *publish-new* and *publish-update* scopes |
| `APPLE_CERTIFICATE` | The Developer ID Application certificate and key, a `.p12`, base64 (`base64 -i DeveloperID.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12`'s password |
| `APPLE_API_KEY` | An App Store Connect API key (`AuthKey_….p8`), base64, for notarising |
| `APPLE_API_KEY_ID`, `APPLE_API_ISSUER` | That key's ID and issuer ID |
| `APPLE_TEAM_ID` | The Apple developer team ID |

The image goes to ghcr.io with the workflow's own token (no secret).

### The Apple certificate and key

With an Apple Developer Program membership, on a Mac:

1. Keychain Access › Certificate Assistant › *Request a Certificate From a
   Certificate Authority*: your email and name, *Saved to disk*.
2. developer.apple.com › Account › Certificates, IDs & Profiles ›
   Certificates › **+** › **Developer ID Application** (G2 Sub-CA; the
   account holder creates it) › upload the request › download the `.cer`.
3. Double-click the `.cer` to add it to your login keychain, where it meets
   the private key from step 1.
4. Keychain Access › My Certificates › *Developer ID Application: … (TEAM)*
   › Export › `.p12` with a password: `APPLE_CERTIFICATE` (base64) and
   `APPLE_CERTIFICATE_PASSWORD`.
5. appstoreconnect.apple.com › Users and Access › Integrations › App Store
   Connect API › Team Keys › **+**, access *Developer* › download the `.p8`
   (only once): `APPLE_API_KEY` (base64), with its Key ID
   (`APPLE_API_KEY_ID`) and the Issuer ID on that page (`APPLE_API_ISSUER`).
6. The team ID is under developer.apple.com › Account › Membership details:
   `APPLE_TEAM_ID`.

The workflow imports the certificate into a temporary keychain, signs
`open-chess-device` with the hardened runtime and a timestamp, and
notarises it with `notarytool`. A bare binary can't carry Apple's ticket,
so a Mac checks it online the first time it runs.
