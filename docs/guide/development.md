# Development

English | [简体中文](development.zh.md)

This guide covers building from source, running locally and checking changes. Installing and using the bridge is covered in the [README](../../README.md). Coding agents also follow [AGENTS.md](../../AGENTS.md).

## Requirements

- Rust 1.99.1, pinned in `rust-toolchain.toml`.
- Bun 1.3.11. Bun installs dependencies and runs all frontend scripts; the repository uses `bun.lock` and no other package manager.
- A C/C++ toolchain and CMake. macOS app builds also need Xcode Command Line Tools.
- OpenSSL, for the independent identity vector check.

Install dependencies from the repository root:

```sh
bun install --frozen-lockfile
```

## Run the web version locally

The management API needs a database and a master key. Create them once in a local state directory, which stays out of version control, and add a management user with the tenant UUID that `init` prints. The first `add-user` asks for an upstream proxy; press Enter for a direct connection.

```sh
mkdir -p -m 700 local-state secrets
cargo run -p bridge-server --bin bridge-admin --locked -- local-state/bridge.sqlite secrets/master.key init
cargo run -p bridge-server --bin bridge-admin --locked -- local-state/bridge.sqlite secrets/master.key add-user TENANT_UUID dev
```

Start the API:

```sh
BRIDGE_DATABASE=local-state/bridge.sqlite \
BRIDGE_MASTER_KEY=secrets/master.key \
BRIDGE_PUBLIC_ORIGIN=http://127.0.0.1:5173 \
cargo run -p bridge-server --locked
```

In a second terminal, start the frontend:

```sh
bun run dev
```

Open http://127.0.0.1:5173. The frontend forwards `/api` to the backend on `127.0.0.1:8787`, so keep both terminals running. Use exactly the host in `BRIDGE_PUBLIC_ORIGIN`: the server rejects `localhost` when the origin says `127.0.0.1`. Without a database and key the page only says the management service is not configured. `bridge-admin` commands need the server stopped, because the server holds the database lock.

## Run the desktop app locally

```sh
bun run desktop:dev
```

This starts the desktop frontend on port 15174 with the app's normal local storage, so it sees the same data as an installed copy.

## Command-line workflows

`bun run bridge:setup` configures and runs a single connection from the terminal, `bun run bridge:start` starts it again, and `bun run bridge:manage` opens a console for several connections. They use the local state directory. `bun run acceptance` starts an isolated instance with its own state and ports for testing with real devices.

## Check changes

Pick the checks that cover what you changed:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
bun run build
bun tests/identity-vectors.ts
bun run --cwd ui test:e2e
bun test tests/package-server.test.ts
```

Rust integration tests bind temporary loopback ports. The Playwright tests run in isolated browser contexts against a synthetic API, so they never touch real accounts. Install their Chromium once before the first run:

```sh
bun --bun ui/node_modules/@playwright/test/cli.js install chromium
```

Keep real tokens, private keys, authorization callbacks, pairing codes and chat payloads out of fixtures, logs and commits.

## Build packages

```sh
bun run desktop:build
bun run package:desktop
bun run package:server
```

The macOS app is written to `target/release/bundle/macos/Codex RC Bridge.app`. Server packages go to `dist/server/`, named after the system and architecture that built them; build Linux packages in a compatible Linux environment.

`package:desktop` builds a DMG for the Mac it runs on, checks the app's signature and writes the installer and SHA-256 file to `dist/desktop/`. It uses ad-hoc signing without an Apple developer account or notarization.

The Docker images and the container test run like this:

```sh
docker build --target test -t codex-rc-bridge-test:local .
docker build --target server -t codex-rc-bridge-server:local .
docker pull oven/bun:1.3.11
bun run test:container
```

`compose.yaml` uses the published `latest` image unless `BRIDGE_IMAGE` says otherwise, and each release attaches a copy pinned to its own version; the container test sets it to `codex-rc-bridge-server:local`. The container test creates a Compose project with a random name, a synthetic management user and its own volumes, publishes only loopback ports, and removes everything when it finishes.

## CI and releases

**Build and release** runs Rust tests, clippy, the frontend build, identity vectors and Playwright tests on pushes, pull requests and manual runs. It then builds macOS DMGs on native Apple Silicon and Intel runners. Pushes and manual runs also build native Linux server packages for x64 and arm64. Download the packages from the run's artifacts; they are kept for 14 days. GitHub shows the **Run workflow** button once the workflow is present on the repository's default branch.

To prepare a release, set the same version in `Cargo.toml` (`workspace.package.version`) and `crates/bridge-desktop/tauri.conf.json`, and refresh `Cargo.lock` with Cargo. Commit those changes before creating a tag such as `v0.1.0`. The tag must match both versions; `bun run check:version` checks the files locally.

Pushing the tag builds all packages and, after every job succeeds, attaches them to a GitHub Release draft together with `compose.yaml` and `compose.env.example`. Review the draft's notes and attachments, then publish it by hand. Tags such as `v0.2.0-beta.1` create prerelease drafts. Rerunning a tag build updates its draft's attachments and refuses to overwrite an already published release. Manual runs only upload artifacts, even when run against a tag. No Apple credentials or extra release token are required; only the release job gets write access through GitHub's built-in token.

Publishing the GitHub Release starts **Publish Docker Hub image**. Before the first release, add a repository Actions secret named `DOCKERHUB_TOKEN` containing a Docker Hub token for `geniucker` with write access to `geniucker/codex-rc-bridge`. The workflow tests and builds `linux/amd64` and `linux/arm64` on native runners, then publishes one multi-platform image. A stable release such as `v0.1.0` gets tags `geniucker/codex-rc-bridge:0.1.0` and `:latest`; prereleases get only their version tag. The image becomes available when that workflow succeeds. Drafts do not publish images, and turning a published prerelease into a full release does not move `latest`; release a new tag instead. If publishing fails, rerun the failed jobs from Actions.
