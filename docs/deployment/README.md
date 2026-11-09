# Deployment

The M1 image is a development build for Linux containers. Build and run it from the repository root.

```sh
docker build -f deploy/Dockerfile.server -t arcdos:local .
docker run --rm --name arcdos-local --read-only --cap-drop=ALL --security-opt=no-new-privileges -p 127.0.0.1:8080:8080 arcdos:local
```

Open [http://127.0.0.1:8080/](http://127.0.0.1:8080/). The image bundles the static frontend at `/opt/arcdos/web` and runs the server as UID and GID 10001. The server listens on port 8080 inside the container. Docker forwards it to loopback on the host. The health check calls the Connect endpoint over the container's loopback interface.

In another terminal, inspect health or view logs.

```sh
docker inspect --format '{{.State.Health.Status}}' arcdos-local
docker logs arcdos-local
```

The status service requires no volumes or secrets. The Proxmox, Compose, Traefik, Authentik, and backup guides will accompany the milestones that add those capabilities.
