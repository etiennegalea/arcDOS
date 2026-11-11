# Deploy on a Proxmox VM

The production Compose stack runs ArcDOS and Traefik on a dedicated Linux VM. Authentik serves a separate HTTPS origin and can run on the same VM in its own Compose project. The optional ONNX worker runs on an internal network with access to its model files and its own credential. Traefik publishes the web ports. The [loopback development stack](README.md) uses a separate Compose project.

## Prepare the VM

1. Create an x86-64 Linux VM in Proxmox VE. Start with four vCPUs and 8 GiB RAM if building both images in the VM. Add a separate virtual data disk with backup enabled and use a VirtIO SCSI controller. Install and enable the QEMU guest agent in the guest and in the VM options. Configure guest time synchronization before using OIDC. [Proxmox VM settings](https://pve.proxmox.com/pve-docs/chapter-qm.html)
2. Install Docker Engine and the Docker Compose plugin in the VM. Use a supported Linux distribution such as Debian and a kernel with `openat2` support. Use an administrator account to operate Docker. [Docker Engine for Debian](https://docs.docker.com/engine/install/debian/)
3. Mount the new data disk at `/srv/arcdos` on a local Linux filesystem such as ext4. Persist the mount by UUID in `/etc/fstab`. Keep the live files and private state together under `/srv/arcdos/data` so ArcDOS can commit atomic renames. Avoid nested mounts under the data directory.
4. On a dedicated systemd guest, add a Docker service drop-in with `sudo systemctl edit docker.service` so a daemon restart waits for the data mount. Use `sudo systemctl daemon-reload` after editing and verify the mount before the first launch.

```ini
[Unit]
RequiresMountsFor=/srv/arcdos
```

From a local checkout of the ArcDOS revision you intend to run, prepare the bind mounts after verifying the disk. These commands assume a Docker daemon without user namespace remapping.

```sh
mountpoint -q /srv/arcdos &&
  sudo install -d -o 10001 -g 10001 -m 0750 /srv/arcdos/data
sudo install -d -m 0755 /etc/arcdos
sudo install -d -o 10002 -g 10002 -m 0700 /etc/arcdos/acme
```

## Configure Authentik and DNS

- Use your own hostnames in place of `drive.example.com` and `auth.example.com`. The browser and the ArcDOS container must resolve and reach Authentik over HTTPS. For a fresh instance, [start the Authentik Compose project and set the administrator password](authentik.md#install-and-start-authentik-with-compose) before starting ArcDOS.
- Configure the confidential web provider, live group claims, MFA, and strict `https://drive.example.com/auth/callback` redirect URI. Save the provider's client secret in `/etc/arcdos/oidc-client-secret`, owned by UID and GID 10001 with mode `0400`. Use a private editor or your secret manager to write it. [ArcDOS provider setup](authentik.md)
- Point the ArcDOS DNS record at the Traefik VM or its NAT address. For the included Let's Encrypt HTTP challenge, TCP 80 must reach Traefik from the internet. Allow TCP 443 for client HTTPS. Publish an AAAA record only if IPv6 works end to end. Restrict management ports at the Proxmox firewall or your network boundary. [Traefik ACME HTTP challenge](https://doc.traefik.io/traefik/reference/install-configuration/tls/certificate-resolvers/acme/#httpchallenge) · [Docker firewall behavior](https://docs.docker.com/engine/network/packet-filtering-firewalls/)

Create `/etc/arcdos/production.env` with the following settings, substituting your DNS name, ACME email address, exact issuer, and client ID. Secret values stay in their own files.

```dotenv
ARCDOS_HOSTNAME=drive.example.com
ARCDOS_ACME_EMAIL=ops@example.com
ARCDOS_OIDC_ISSUER=https://auth.example.com/application/o/arcdos-web/
ARCDOS_OIDC_CLIENT_ID=replace-with-web-client-id
```

## Route local Authentik through Traefik

If you run the Authentik Compose project on this VM and want the bundled Traefik to serve its HTTPS origin, point the Authentik DNS record at the same Traefik address as the ArcDOS record. Add `AUTHENTIK_HOSTNAME=auth.example.com` to `/etc/arcdos/production.env`. Use the same hostname in `ARCDOS_OIDC_ISSUER`. When setting up the provider over the SSH tunnel, form the issuer from the final HTTPS origin and application slug, as in the example above.

Use this function in place of the base `arcdos` function below. Start the proxy first so the provider's HTTPS discovery endpoint is available before ArcDOS starts.

```sh
arcdos() {
  sudo docker compose --env-file /etc/arcdos/production.env \
    -f deploy/compose.production.yml -f deploy/compose.authentik-proxy.yml "$@"
}

arcdos config --quiet
arcdos up --detach --no-deps proxy
curl --fail --silent --show-error \
  https://auth.example.com/application/o/arcdos-web/.well-known/openid-configuration
```

Run the discovery check after DNS resolves and Traefik has obtained a certificate. Confirm the returned issuer matches `ARCDOS_OIDC_ISSUER`. Keep the Authentik project running and include the overlay in subsequent ArcDOS Compose commands. If you use an existing HTTPS endpoint for Authentik, proceed with the base stack below.

## Start and verify

Run from the repository root. If you defined `arcdos` with the Authentik overlay above, keep that function and skip this base definition. Use the same Compose files and environment file for subsequent operations.

```sh
arcdos() {
  sudo docker compose --env-file /etc/arcdos/production.env \
    -f deploy/compose.production.yml "$@"
}
```

```sh
arcdos config --quiet
arcdos up --build --detach --wait --wait-timeout 120
arcdos ps
arcdos logs --tail 100 server proxy
```

Traefik routes the entire HTTPS origin to `server:8080` using the file provider. It keeps access logging off so OIDC callback parameters stay out of access logs. The ACME directory contains account and TLS private keys. Keep its host permissions and protect copies in backups. If your site already terminates HTTPS elsewhere, use the private upstream pattern in [Authentik setup](authentik.md) and route the full origin through your existing proxy. [Traefik file provider](https://doc.traefik.io/traefik/providers/file/)

Verify the HTTPS redirect, trusted certificate, and public readiness endpoint. Run the following from a client that resolves the actual hostname, with the example name replaced.

```sh
curl --fail --silent --show-error --head http://drive.example.com/
curl --fail --silent --show-error \
  -H 'Content-Type: application/json' -H 'Connect-Protocol-Version: 1' \
  --data '{}' https://drive.example.com/arcdos.account.v1.HealthService/Check
```

Open `https://drive.example.com/files/`, sign in with an authorized MFA-enrolled test account, and upload, download, and trash a synthetic file. Check that a user without either allowed group is denied, and that a direct file download URL is denied in a private window. Check logs for provider discovery or TLS errors if readiness succeeds but sign-in fails.

## Optional inference

Prepare the pinned model and verify its checksums using [local AI setup](local-ai.md#prepare-the-worker). Use only its model preparation steps because Compose manages the worker container and network here. Generate a distinct `/etc/arcdos/ai-token` with a random 32-byte value encoded as hexadecimal. Give UID and GID 10001 read access with mode `0400`. Keep the model files readable by UID 10001 under `/srv/arcdos/models/minilm`.

Use the overlay for each stack operation after enabling inference. If using the Authentik overlay, keep it in the file list alongside the AI overlay, including when disabling inference.

```sh
arcdos() {
  sudo docker compose --env-file /etc/arcdos/production.env \
    -f deploy/compose.production.yml -f deploy/compose.ai.yml "$@"
}

arcdos config --quiet
arcdos up --build --detach --wait --wait-timeout 180
arcdos ps
```

The server can start independently of worker health. Test worker isolation with `arcdos stop ai-worker`, check that keyword search and file downloads still work, then run `arcdos start ai-worker`. To permanently disable inference, stop the worker, switch back to the base Compose command, and run `arcdos up --detach --remove-orphans`.

## Operate and recover

- Use `arcdos stop` before backup or VM maintenance. Compose stops ingress before the server. Traefik gives active requests time to drain and ArcDOS stops accepting requests and finishes active work until its stop grace period expires. Transfers longer than the stop window can be interrupted. Resume interrupted uploads after startup. The worker can discard an in-progress batch because the server retries embedding work.
- Check `arcdos ps`, `arcdos logs --tail 100`, and the data disk's free space. Docker health status diagnoses readiness. The restart policy restarts processes that exit. Check the HTTPS endpoint and sign-in separately from container health.
- For upgrades or secret rotation, take a restorable backup first. Recreate the affected containers to load new mounted secret files. After a provider signing-key rotation, restart ArcDOS so it reloads discovery and signing keys. Keep the checkout revision and deployed settings with your recovery records.
- Follow [backup and restore](backup-restore.md) for the data mount and VM recovery procedure. Preserve Authentik's database and credentials using its own backup process. Protect ArcDOS's settings, credential files, and ACME directory in a separate encrypted backup. Validate a restore on an isolated VM before routing production traffic to it. [Authentik backup](https://docs.goauthentik.io/sys-mgmt/ops/backup-restore/)
