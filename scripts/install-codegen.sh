#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)-$(uname -m)" != Linux-x86_64 ]]; then
    printf '%s\n' 'This installer supports x86-64 Linux. See docs/development.md for the required tools.' >&2
    exit 1
fi

root_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tool_dir="$root_dir/.local-tools/bin"
download_dir=$(mktemp -d)
trap 'rm -f -- "$download_dir"/*; rmdir -- "$download_dir"' EXIT
mkdir -p -- "$tool_dir"

download() {
    local url=$1 expected_sha256=$2 name=$3
    curl --fail --silent --show-error --location --retry 3 --output "$download_dir/$name" "$url"
    printf '%s  %s\n' "$expected_sha256" "$download_dir/$name" | sha256sum --check --status
}

download https://github.com/bufbuild/buf/releases/download/v1.75.0/buf-Linux-x86_64 \
    fad1e38d6909bbb583a4da81d8c02a01046e2cd663bde7e337dd24277393764b buf
download https://github.com/anthropics/buffa/releases/download/v0.9.2/protoc-gen-buffa-v0.9.2-linux-x86_64 \
    e8d87b0cc34beb5524835888da694901deeeccaefd834fd5c03300375b629688 protoc-gen-buffa
download https://github.com/anthropics/buffa/releases/download/v0.9.2/protoc-gen-buffa-packaging-v0.9.2-linux-x86_64 \
    965e5b7b2ef6847f6597d32565468a093d70ab82ee74c326ff5397b04d419161 protoc-gen-buffa-packaging
download https://github.com/connectrpc/connect-rust/releases/download/v0.9.1/protoc-gen-connect-rust-v0.9.1-linux-x86_64 \
    580c2a0690b9ad48364bd455cf0b7f8f0153bcbffe85140dd2ce07bfc0b1da24 protoc-gen-connect-rust
download https://github.com/protocolbuffers/protobuf/releases/download/v36.2/protoc-36.2-linux-x86_64.zip \
    121f6c7afe1d4d0e3ea6aab9432038599250134cbf4474cb1167d2c7decd4278 protoc.zip
download https://github.com/connectrpc/connect-kotlin/releases/download/v0.10.0/protoc-gen-connect-kotlin-0.10.0.jar \
    30d166ba87b6c9a4dfa501a2241f23b0c6523960828a798bce45294988ccc65b protoc-gen-connect-kotlin-0.10.0.jar

unzip -p "$download_dir/protoc.zip" bin/protoc > "$download_dir/protoc"
install -m 755 -- "$download_dir/"{buf,protoc,protoc-gen-buffa,protoc-gen-buffa-packaging,protoc-gen-connect-rust} "$tool_dir/"
install -m 644 -- "$download_dir/protoc-gen-connect-kotlin-0.10.0.jar" "$tool_dir/"
printf 'Code generators installed in %s\n' "$tool_dir"
