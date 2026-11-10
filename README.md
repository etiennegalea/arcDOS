# ArcDOS

ArcDOS is an open-source, self-hosted file manager for a shared Linux filesystem. The web workspace supports browsing, previews, file operations, resumable transfers, and keyword search. An optional local AI worker adds semantic document ranking. [Local AI setup](docs/deployment/local-ai.md)

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

## Run with Docker Compose

```sh
docker compose up --build --wait
```

Open [http://127.0.0.1:8080/](http://127.0.0.1:8080/). Run `docker compose down` to stop and remove the container. See [the deployment guide](docs/deployment/README.md) for port configuration and management commands.

## Development

The `crates/server` crate implements the service and serves the built frontend in `apps/web`. The `crates/protocol` crate and the generated browser bindings come from `proto/`. Start with [the architecture overview](docs/architecture/overview.md) and [the development guide](docs/development.md) for checks, binding generation, and browser tests.

The default instance serves the status page and public health check. Enable the Linux file API with Authentik to let authorized users access the shared filesystem. For local development, use a private instance with synthetic data and a development token. [Authentik setup](docs/deployment/authentik.md) · [Local file API setup](docs/deployment/README.md#enable-the-linux-file-api) · [File manager guide](docs/file-manager.md) · [Roadmap](docs/roadmap.md)

ArcDOS is licensed under [MIT](LICENSE).
