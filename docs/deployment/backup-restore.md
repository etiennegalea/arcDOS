# Back up and restore ArcDOS

Use a stopped Proxmox VM backup that includes the guest system disk and the entire virtual disk mounted at `/srv/arcdos`. Keep `/srv/arcdos/data/files` and `/srv/arcdos/data/state` together. The catalog, trash, upload staging, SQLite journals, and search indexes describe the same filesystem at that point in time. Keep each SQLite database with its companion files. [Proxmox backup and restore](https://pve.proxmox.com/pve-docs/pve-admin-guide.pdf) · [SQLite WAL files](https://www.sqlite.org/wal.html#the_wal_file)

ArcDOS associates opaque IDs with Linux device and inode numbers and filesystem creation timestamps. A VM disk backup preserves the on-disk identity fields. Check the Linux device number after restoring because the kernel assigns it when the guest boots. A file archive or ordinary copy creates new inodes. Use that format for individual file recovery into a fresh catalog.

## Take a cold backup

1. Confirm both virtual disks are included in the Proxmox VM backup. Disable other writers to `/srv/arcdos/data`. Use the `arcdos` Compose command from the production guide with the same files and settings as the running stack. Stop the stack and verify that `arcdos ps --status running` shows no services.
2. Record the deployed Git revision, the VM disk/controller layout, and the following output in the protected backup record. Substitute your data path if you changed it in Compose.

   ```sh
   mountpoint -q /srv/arcdos &&
     findmnt --noheadings --output SOURCE,UUID,FSTYPE --target /srv/arcdos/data &&
     stat --format='device=%d root_inode=%i birth=%w' /srv/arcdos/data/files
   ```

3. Shut down the guest normally. In Proxmox, select the VM and use **Backup → Backup now**, with **Mode → Stop**. Save the archive to backup storage separate from the VM disks and verify the completed job. Retain at least one copy outside the primary host.
4. Start the original VM and then start the Compose stack. Keep the backup private and encrypted at rest. It includes user data, OAuth session state, and, on the system disk, the deployment settings, mounted secrets, and Traefik ACME keys. Back up the separate Authentik deployment according to its own recovery procedure. [Production operation](production.md#operate-and-recover) · [Authentik backup](https://docs.goauthentik.io/install-config/backup-restore/)

## Restore on an isolated VM

1. Restore the backup to an unused VM ID with the original virtual disk layout. Disconnect its virtual NIC before first boot. Use the Proxmox console and keep the production VM and original disk unchanged during the drill.
2. Before starting Compose, verify that the filesystem UUID, decimal Linux device number, root inode, and birth time match the backup record. Inspect the files and state directories at the restored mount. A device number mismatch requires correcting the guest disk configuration or using file-only recovery. Keep the server stopped on a mismatch so reconciliation cannot replace live IDs in the catalog.
3. Review the restored secrets against the active Authentik and worker settings. Clear the restored browser sessions before accepting traffic. With the stack stopped, move `state/auth.sqlite3` and any `state/auth.sqlite3-wal`, `state/auth.sqlite3-shm`, and `state/auth.sqlite3-journal` companions together out of the data directory to a private recovery location. ArcDOS creates a fresh session database at startup and users sign in again. Keep the metadata and transfer databases in place.
4. Connect the clone only to a test network. Configure a test DNS name, TLS, and Authentik client for the clone. Start the restored Git revision with the restored data mount. Check health, MFA sign-in, and group authorization. Open a known file by its saved ID, compare its downloaded SHA-256, check a known tag and search match, and restore a synthetic item from trash. If resumable uploads matter to your deployment, verify that a staged synthetic upload still reports the saved offset. Record the tested backup and result.
5. For a production cutover, ensure only the intended VM can receive production traffic. If clients had already synchronized past the backup point, reset their local journal cursors and refresh from the restored server.

## Recover individual files

When restoring only user files, export them from the backup and place them in `files/` on a freshly initialized data mount. Recover selected trash content separately if needed. ArcDOS discovers the files and assigns new opaque IDs. Existing links, tags, change cursors, and upload sessions from the old catalog require the whole-VM recovery procedure.

## Exercise a synthetic restore

On a disposable Linux host, use a built ArcDOS server binary and run the following as root. The script needs ext4 and loop devices, `mkfs.ext4`, `losetup`, and curl. It creates and copies a temporary ext4 image and verifies the restored download checksum, indexed tag, and original live and trash IDs through the server.

```sh
sudo bash scripts/test-cold-restore.sh target/debug/arcdos-server
```
