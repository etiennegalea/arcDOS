# ArcDOS

ArcDOS is an open-source, self-hosted file manager for a shared Linux filesystem, with planned local AI indexing inspired by FilDOS. This repository currently implements M1. The status page verifies the connection from the SvelteKit browser client to the Rust Axum server through Connect RPC and binary Protobuf.

## Run locally

Install Rust through [rustup](https://rustup.rs/) with the native linker for your platform, and use Node.js 24.22.2 LTS. The workspace pins Rust 1.99.1. Run these commands from the repository root.

```sh
npm --prefix apps/web ci --no-audit --no-fund
npm --prefix apps/web run build
cargo run --locked --bin arcdos-server
```

Open [http://127.0.0.1:8080/](http://127.0.0.1:8080/) and check that the API reports ready. The server uses `apps/web/build` by default.

| Variable | Native default | Purpose |
| --- | --- | --- |
| `ARCDOS_BIND` | `127.0.0.1:8080` | Socket address for the server |
| `ARCDOS_WEB_DIR` | `apps/web/build` | Directory containing the static web build |
| `ARCDOS_API_ORIGIN` | `http://127.0.0.1:8080` | Vite development proxy target |

## Run with Docker

```sh
docker build -f deploy/Dockerfile.server -t arcdos:local .
docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges -p 127.0.0.1:8080:8080 arcdos:local
```

The image runs as a non-root user and checks the health RPC. See [the deployment guide](docs/deployment/README.md) for the container settings.

## Development

The `crates/server` crate implements the service and serves the built frontend in `apps/web`. The `crates/protocol` crate and the generated browser bindings come from `proto/`. Start with [the architecture overview](docs/architecture/overview.md) and [the development guide](docs/development.md) for checks, binding generation, and browser tests.

M1 exposes the status page and a public health check for local development. File storage, Authentik authorization, search, AI inference, and device synchronization are tracked in [the roadmap](docs/roadmap.md). Authentik setup will ship with M4.

ArcDOS is licensed under [MIT](LICENSE).
