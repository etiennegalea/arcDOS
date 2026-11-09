# Development

Run commands from the repository root. Use the version in `rust-toolchain.toml` and Node.js 24.22.2. On Windows, install the linker that matches your Rust host toolchain.

## Build and check

```sh
npm --prefix apps/web ci --no-audit --no-fund
npm --prefix apps/web run lint
npm --prefix apps/web run check
npm --prefix apps/web run build
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

For live UI changes, run `cargo run --locked --bin arcdos-server` and `npm --prefix apps/web run dev` in separate terminals. Open the URL printed by Vite. Its proxy forwards Connect requests to Axum.

## Regenerate Protobuf bindings

Use Linux x86-64 or WSL with Bash, Java 17 or later, curl, unzip, and the Rust and Node toolchains. Generation uses pinned local plugins. Routine builds use the checked-in Rust and TypeScript output.

```sh
npm --prefix apps/web ci --no-audit --no-fund
bash scripts/install-codegen.sh
export PATH="$PWD/.local-tools/bin:$PATH"
buf format --diff --exit-code
buf lint
buf generate
buf breaking --against '.git#branch=main'
```

Run the breaking check once the comparison branch contains the Buf module. Commit updated Rust and TypeScript bindings with the schema. The Kotlin and Java Lite output is generated under `gen/android/`.

## Browser integration test

Build the frontend and run the server, or start the Docker image on loopback port 8080. Install Chromium and run Playwright against the running application.

```sh
cd apps/web
npx playwright install chromium
npm run test:e2e
```

Playwright defaults to `http://127.0.0.1:8080`. Set `ARCDOS_E2E_URL` to test another instance. Set `ARCDOS_E2E_BROWSER_CHANNEL=msedge` to use an installed Microsoft Edge browser. On a fresh Linux CI host use `npx playwright install --with-deps chromium`.

To run the file manager's live tests, enable the Linux file API on a disposable instance with synthetic storage and set `ARCDOS_E2E_DEV_TOKEN` to that instance's development token. The tests create, upload, move to trash, and restore their own fixtures. The health and isolated frontend tests also run against the status-only image.

GitHub Actions validates schema format and compatibility, regenerates bindings, runs Rust and Svelte checks, builds the Linux image, and runs Playwright against it.
