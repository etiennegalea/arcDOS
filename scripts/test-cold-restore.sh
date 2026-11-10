#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || "$(id -u)" != 0 ]]; then
    echo "Usage (Linux root on a disposable test host): bash $0 /path/to/arcdos-server" >&2
    exit 2
fi
server=$(realpath -- "$1")
test -x "$server"
fixture=$(mktemp -d /tmp/arcdos-cold-restore.XXXXXXXX)
mount_dir="$fixture/disk"
mkdir "$mount_dir" "$fixture/web"
loop_device=
server_pid=

stop_server() {
    if [[ -n "$server_pid" ]]; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid"
        server_pid=
    fi
}

cleanup() {
    stop_server || true
    if mountpoint -q "$mount_dir"; then
        umount "$mount_dir" || return
    fi
    if [[ -n "$loop_device" ]]; then
        case "$(losetup --list --noheadings --output BACK-FILE "$loop_device" 2>/dev/null)" in
            "$fixture/source.img"|"$fixture/restored.img") losetup -d "$loop_device" ;;
        esac
    fi
    rm -rf -- "$fixture"
}
trap cleanup EXIT

data="$mount_dir/data"
token=synthetic-restore-fixture-token-0000
printf '%s\n' "$token" > "$fixture/token"
chmod 0600 "$fixture/token"

start_server() {
    env -i PATH="$PATH" ARCDOS_BIND=127.0.0.1:0 ARCDOS_WEB_DIR="$fixture/web" \
        ARCDOS_DATA_DIR="$data" ARCDOS_DEV_TOKEN_FILE="$fixture/token" "$server" \
        > "$fixture/server.log" 2>&1 &
    server_pid=$!
    for _ in {1..100}; do
        address=$(sed -n 's/.*"address":"\(127\.0\.0\.1:[0-9]*\)".*/\1/p' "$fixture/server.log")
        if [[ -n "$address" ]]; then return; fi
        if ! kill -0 "$server_pid" 2>/dev/null; then break; fi
        sleep .05
    done
    cat "$fixture/server.log" >&2
    return 1
}

rpc() {
    curl --fail-with-body --silent --show-error \
        -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
        -H 'Connect-Protocol-Version: 1' --data "$2" "http://$address/$1"
}

json_string() {
    sed -nE "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"([^\"]*)\".*/\1/p"
}

file_rpc=arcdos.files.v1.FileService
search_rpc=arcdos.search.v1.SearchService
truncate -s 64M "$fixture/source.img"
mkfs.ext4 -q -F "$fixture/source.img"
loop_device=$(losetup --find --show "$fixture/source.img")
mount "$loop_device" "$mount_dir"
mkdir -p "$data/files"
printf 'trash fixture\n' > "$data/files/trash.txt"
start_server

entry=$(rpc "$file_rpc/ListEntries" '{}')
trash_id=$(json_string entryId <<< "$entry")
trash_revision=$(json_string revision <<< "$entry")
test -n "$trash_id" && test -n "$trash_revision"
rpc "$file_rpc/TrashEntry" "{\"entryId\":\"$trash_id\",\"expectedRevision\":\"$trash_revision\"}" > /dev/null

printf 'synthetic restore bytes\n' > "$data/files/survives.txt"
hash=$(sha256sum "$data/files/survives.txt" | cut -d ' ' -f 1)
entry=$(rpc "$file_rpc/ListEntries" '{}')
live_id=$(json_string entryId <<< "$entry")
live_revision=$(json_string revision <<< "$entry")
test -n "$live_id" && test -n "$live_revision"
rpc "$search_rpc/SetTags" "{\"entryId\":\"$live_id\",\"expectedRevision\":\"$live_revision\",\"tags\":[\"surviving-tag\"]}" > /dev/null
stop_server

identity=$(stat --format='%d %i %w' "$data/files")
umount "$mount_dir"
losetup -d "$loop_device"
cp --sparse=always "$fixture/source.img" "$fixture/restored.img"
losetup "$loop_device" "$fixture/restored.img"
mount "$loop_device" "$mount_dir"
test "$(stat --format='%d %i %w' "$data/files")" = "$identity"
start_server

actual_hash=$(curl --fail --silent --show-error -H "Authorization: Bearer $token" \
    "http://$address/api/files/v1/$live_id/content" | sha256sum | cut -d ' ' -f 1)
test "$actual_hash" = "$hash"
for _ in {1..100}; do
    search=$(rpc "$search_rpc/SearchFiles" '{"query":"surviving tag"}')
    if [[ "$search" == *'"surviving-tag"'* ]]; then break; fi
    sleep .05
done
[[ "$search" == *'"surviving-tag"'* && "$search" == *"\"$live_id\""* ]]
trash=$(rpc "$file_rpc/ListTrash" '{}')
test "$(json_string entryId <<< "$trash")" = "$trash_id"
trash_revision=$(json_string revision <<< "$trash")
restored=$(rpc "$file_rpc/RestoreEntry" "{\"entryId\":\"$trash_id\",\"expectedRevision\":\"$trash_revision\"}")
test "$(json_string entryId <<< "$restored")" = "$trash_id"
test "$(cat "$data/files/trash.txt")" = "trash fixture"

echo "Cold restore preserved live and trashed IDs, download checksum, and indexed tags."
