# Deployment

For an authorized instance with HTTPS on a Proxmox VM, use the [production guide](production.md). It covers Authentik, Traefik, optional inference, and [backup and restore](backup-restore.md).

The development image runs in Linux containers. Build and run it with Docker Compose from the repository root.

```sh
docker compose up --build --wait
```

Open [http://127.0.0.1:8080/](http://127.0.0.1:8080/). Compose publishes the server on the host's loopback interface. Set `ARCDOS_HOST_PORT` in your shell or a root `.env` file to change the host port. The server listens on port 8080 inside the container.

The image runs as UID and GID 10001 and checks the Connect health RPC. Compose runs it with a read-only root filesystem and restarts it after failures or daemon restarts unless it was manually stopped.

| Command | Effect |
| --- | --- |
| `docker compose ps` | Check status and health |
| `docker compose logs -f server` | Follow server logs |
| `docker compose stop` | Stop the container and keep it for later |
| `docker compose start` | Start a stopped container |
| `docker compose down` | Stop and remove the container and its Compose network |

The status service requires no volumes or secrets. To run the file manager behind HTTPS with account and group access, configure Authentik and mount the shared filesystem and confidential client secret. [Authentik setup](authentik.md)

## Enable the Linux file API

Use development token mode on a private loopback test instance with synthetic data. Linux 5.6 or newer with procfs and a local filesystem supporting directory synchronization and atomic no-replace rename is required. Mount one writable directory at `/data` so `/data/files` and `/data/state` share a mount. Use a separate read-only token file.

For a synthetic test on a Linux Docker host, run from the repository root.

```sh
test_dir="$(mktemp -d)"
sudo install -d -o 10001 -g 10001 -m 0750 "$test_dir/data"
openssl rand -hex 32 > "$test_dir/token"
sudo chown 10001:10001 "$test_dir/token"
sudo chmod 0600 "$test_dir/token"

docker build -f deploy/Dockerfile.server -t arcdos:local .
docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  -p 127.0.0.1:8081:8080 \
  --mount "type=bind,source=$test_dir/data,target=/data" \
  --mount "type=bind,source=$test_dir/token,target=/run/arcdos-dev-token,readonly" \
  -e ARCDOS_DATA_DIR=/data \
  -e ARCDOS_DEV_TOKEN_FILE=/run/arcdos-dev-token \
  arcdos:local
```

The process initializes `files/` and private `state/` beneath the data root. Keep that directory away from the web assets and give it to one ArcDOS server process. Open [the file manager](http://127.0.0.1:8081/files/) and enter the token from `"$test_dir/token"`. For direct API calls, send the token in the `Authorization` header. [File manager guide](../file-manager.md) · [File API](../api/files-v1.md)

To enable semantic ranking on an authorized file server, add the optional [local AI worker](local-ai.md).
