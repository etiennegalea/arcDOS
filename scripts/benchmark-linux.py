#!/usr/bin/env python3
"""Measure ArcDOS against a disposable synthetic Linux file store."""

import argparse
import hashlib
import http.client
import json
import math
import os
import platform
import queue
import signal
import sqlite3
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, closing
from pathlib import Path


FILES = "/arcdos.files.v1.FileService/"
SEARCH = "/arcdos.search.v1.SearchService/SearchFiles"
HEALTH = "/arcdos.account.v1.HealthService/Check"
TOKEN = "arcdos-benchmark-only-fixture-token-00000000000000"


def png_fixture():
    width, height = 64, 48
    pixels = b"".join(b"\0" + bytes((x + y) % 256 for x in range(width)) for y in range(height))

    def chunk(kind, payload):
        packet = kind + payload
        return struct.pack("!I", len(payload)) + packet + struct.pack("!I", zlib.crc32(packet))

    header = struct.pack("!IIBBBBB", width, height, 8, 0, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b"")


def pdf_fixture():
    stream = b"BT /F1 12 Tf 20 60 Td (Synthetic ArcDOS benchmark) Tj ET\n"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 320 120] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        f"<< /Length {len(stream)} >>\nstream\n".encode() + stream + b"endstream",
    ]
    data = bytearray(b"%PDF-1.4\n")
    offsets = []
    for index, obj in enumerate(objects, 1):
        offsets.append(len(data))
        data.extend(f"{index} 0 obj\n".encode() + obj + b"\nendobj\n")
    start = len(data)
    data.extend(f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode())
    for offset in offsets:
        data.extend(f"{offset:010d} 00000 n \n".encode())
    data.extend(f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n".encode())
    return data


def seed(root, count, transfer_mib):
    flat = root / "flat"
    flat.mkdir(parents=True)
    for i in range(count):
        (flat / f"file-{i:06}.txt").write_text(f"synthetic document {i:06}\n", encoding="utf-8")
    deep = root / "deep"
    for i in range(32):
        deep /= f"d{i:02}"
    deep.mkdir(parents=True)
    (deep / "leaf.txt").write_text("synthetic deep leaf\n", encoding="utf-8")
    media = root / "media"
    media.mkdir()
    png, pdf = png_fixture(), pdf_fixture()
    for i in range(20):
        (media / f"image-{i:02}.png").write_bytes(png)
        (media / f"page-{i:02}.pdf").write_bytes(pdf)
    block = bytes(range(256)) * 4096
    digest = hashlib.sha256()
    with (media / "transfer.bin").open("wb") as transfer:
        for _ in range(transfer_mib):
            transfer.write(block)
            digest.update(block)
    total = sum(len(directories) + len(files) for _, directories, files in os.walk(root))
    return total, digest.hexdigest()


class Client:
    def __init__(self, port):
        self.connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.connection.close()

    def rpc(self, method, data):
        self.connection.request("POST", method, json.dumps(data), {
            "Authorization": f"Bearer {TOKEN}",
            "Connect-Protocol-Version": "1",
            "Content-Type": "application/json",
        })
        with self.connection.getresponse() as response:
            body = response.read()
            if response.status != 200:
                raise RuntimeError(f"{method} returned {response.status}: {body[:512]!r}")
            return json.loads(body)

    def listing(self, parent="", limit=100, cursor=""):
        return self.rpc(FILES + "ListEntries", {
            "parentId": parent, "pageSize": limit, "pageToken": cursor,
        })

    def download(self, entry_id):
        self.connection.request("GET", f"/api/files/v1/{entry_id}/content", headers={
            "Authorization": f"Bearer {TOKEN}",
        })
        size = 0
        digest = hashlib.sha256()
        with self.connection.getresponse() as response:
            if response.status != 200:
                raise RuntimeError(f"download returned {response.status}")
            while block := response.read(64 * 1024):
                size += len(block)
                digest.update(block)
        return size, digest.hexdigest()


def timed(operation):
    before = time.perf_counter()
    value = operation()
    return value, (time.perf_counter() - before) * 1_000


def summary(samples):
    ordered = sorted(samples)
    return {
        "samples": len(samples),
        "p50_ms": round(statistics.median(samples), 3),
        "p95_ms": round(ordered[math.ceil(0.95 * len(ordered)) - 1], 3),
        "max_ms": round(ordered[-1], 3),
    }


def measure(port, samples, clients, operation):
    barrier = threading.Barrier(clients + 1, timeout=30)

    def lane(client, lane_id):
        barrier.wait()
        return [timed(lambda: operation(client, index))[1]
                for index in range(lane_id, samples, clients)]

    with ExitStack() as connections, ThreadPoolExecutor(max_workers=clients) as pool:
        peers = [connections.enter_context(Client(port)) for _ in range(clients)]
        for client in peers:
            client.rpc(HEALTH, {})
        runs = [pool.submit(lane, client, lane_id) for lane_id, client in enumerate(peers)]
        start = time.perf_counter()
        barrier.wait()
        result = summary([sample for run in runs for sample in run.result()])
        result["requests_per_second"] = round(samples / (time.perf_counter() - start), 2)
        return result


def entry_named(listing, name):
    return next(entry for entry in listing["entries"] if entry["name"] == name)


def wait_for_index(data, total, timeout):
    db = (data / "state/metadata.db").as_uri() + "?mode=ro"
    start = time.perf_counter()
    next_report = start + 30
    while True:
        try:
            with closing(sqlite3.connect(db, uri=True, timeout=1)) as conn:
                count = conn.execute("SELECT count(*) FROM search_state WHERE indexed_revision = revision").fetchone()[0]
                pending = conn.execute("SELECT count(*) FROM index_jobs").fetchone()[0]
        except sqlite3.OperationalError as error:
            if "locked" not in str(error):
                raise
        else:
            if count >= total and pending == 0:
                return round(time.perf_counter() - start, 3)
            if time.perf_counter() >= next_report:
                print(f"Keyword index: {count}/{total} entries, {pending} pending", file=sys.stderr)
                next_report += 30
        if time.perf_counter() - start > timeout:
            raise TimeoutError(f"Keyword index did not reach {total} entries within {timeout}s")
        time.sleep(0.25)


def process_memory(pid):
    status = Path(f"/proc/{pid}/status").read_text(encoding="ascii")
    keys = {"VmRSS": "rss_kib", "VmHWM": "peak_rss_kib"}
    return {keys[key]: int(value.split()[0]) for line in status.splitlines()
            for key, _, value in [line.partition(":")] if key in keys}


def embedded_chunks(data):
    database = data / "state/semantic.db"
    if not database.is_file():
        return 0
    with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=1)) as conn:
        initialized = conn.execute("SELECT 1 FROM sqlite_master WHERE name='embeddings'").fetchone()
        return conn.execute("SELECT count(*) FROM embeddings").fetchone()[0] if initialized else 0


def benchmark(args, port, pid, data, total, digest, startup):
    with Client(port) as client:
        root = client.listing()
        flat = entry_named(root, "flat")["entryId"]
        first, cold_list = timed(lambda: client.listing(flat))
        media = entry_named(root, "media")["entryId"]
        transfer = entry_named(client.listing(media), "transfer.bin")
        leaf = entry_named(root, "deep")["entryId"]
        started = time.perf_counter()
        for i in range(32):
            leaf = entry_named(client.listing(leaf), f"d{i:02}")["entryId"]
        entry_named(client.listing(leaf), "leaf.txt")
        deep_ms = round((time.perf_counter() - started) * 1_000, 3)

    page_size = min(args.files, 100)
    if len(first["entries"]) != page_size:
        raise RuntimeError("Unexpected first-page length")
    index_wait = wait_for_index(data, total, args.index_timeout)

    def listing(client, _):
        page = client.listing(flat)
        if len(page["entries"]) != page_size or page["entries"][0]["name"] != "file-000000.txt":
            raise RuntimeError("Unexpected first-page entries")

    def search(client, index):
        item = (index * 1_271) % args.files
        results = client.rpc(SEARCH, {"query": f"file {item:06}", "limit": 10})
        if not any(hit["entry"]["name"] == f"file-{item:06}.txt" for hit in results.get("hits", [])):
            raise RuntimeError("Indexed filename was missing from search results")

    result = {
        "environment": {
            "kernel": platform.release(),
            "architecture": platform.machine(),
            "logical_cpus": len(os.sched_getaffinity(0)),
            "python": platform.python_version(),
        },
        "files_in_flat_directory": args.files,
        "total_indexed_entries": total,
        "server_startup_ms": round(startup, 3),
        "first_flat_page_ms": round(cold_list, 3),
        "deep_32_level_navigation_ms": deep_ms,
        "keyword_index_wait_seconds": index_wait,
        "semantic_worker_configured": args.ai_url is not None,
        "warm": {},
    }
    if args.ai_url is not None:
        result["embedded_chunks_before_warm_samples"] = embedded_chunks(data)
    for clients in sorted({1, args.clients}):
        result["warm"][str(clients)] = {
            "list_first_page": measure(port, args.samples, clients, listing),
            "filename_search": measure(port, args.samples, clients, search),
        }
        if cursor := first.get("nextPageToken"):
            def second_page(client, _):
                page = client.listing(flat, cursor=cursor)
                if page["entries"][0]["name"] != "file-000100.txt":
                    raise RuntimeError("Directory pagination lost its cursor")

            result["warm"][str(clients)]["list_second_page"] = measure(port, args.samples, clients, second_page)
    with Client(port) as client:
        (size, actual_digest), duration = timed(lambda: client.download(transfer["entryId"]))
    if size != args.transfer_mib * 1024**2 or actual_digest != digest:
        raise RuntimeError("Downloaded data differs from the synthetic fixture")
    result["download_mib_per_second"] = round(args.transfer_mib * 1_000 / duration, 2)
    if args.ai_url is not None:
        result["embedded_chunks_after_warm_samples"] = embedded_chunks(data)
    result["server_memory"] = process_memory(pid)
    return result


def run(args, scratch):
    data = scratch / "data"
    token = scratch / "development-token"
    token.write_text(TOKEN, encoding="ascii")
    total, digest = seed(data / "files", args.files, args.transfer_mib)
    env = os.environ.copy()
    for name in tuple(env):
        if name.startswith("ARCDOS_"):
            del env[name]
    env.update(
        ARCDOS_DATA_DIR=str(data), ARCDOS_DEV_TOKEN_FILE=str(token),
        ARCDOS_WEB_DIR=str(args.web_dir), ARCDOS_BIND="127.0.0.1:0", RUST_LOG="info",
    )
    if args.ai_url is not None:
        env.update(ARCDOS_AI_WORKER_URL=args.ai_url, ARCDOS_AI_TOKEN_FILE=str(args.ai_token_file))
    before = time.perf_counter()
    with (scratch / "server.stderr").open("w+") as errors, subprocess.Popen(
        [str(args.server)], env=env, stdout=subprocess.PIPE, stderr=errors, text=True,
    ) as server:
        addresses = queue.Queue(maxsize=1)

        def read_log():
            for line in server.stdout:
                try:
                    fields = json.loads(line)["fields"]
                    if fields.get("message") == "ArcDOS listening":
                        addresses.put_nowait(int(fields["address"].rsplit(":", 1)[1]))
                except (ValueError, KeyError, queue.Full):
                    continue
            try:
                addresses.put_nowait(None)
            except queue.Full:
                pass

        log_thread = threading.Thread(target=read_log, daemon=True)
        log_thread.start()
        try:
            try:
                port = addresses.get(timeout=30)
            except queue.Empty as error:
                raise TimeoutError("ArcDOS did not start listening within 30 seconds") from error
            if port is None:
                errors.seek(0)
                raise RuntimeError(f"ArcDOS exited during startup: {errors.read()[-2048:]}")
            with Client(port) as client:
                if not client.rpc(HEALTH, {}).get("ready"):
                    raise RuntimeError("ArcDOS did not report ready")
            startup = (time.perf_counter() - before) * 1_000
            if args.serve:
                wait_for_index(data, total, args.index_timeout)
                print(f"export ARCDOS_E2E_URL=http://127.0.0.1:{port}\nexport ARCDOS_E2E_DEV_TOKEN={TOKEN}\n"
                      f"export ARCDOS_BENCH_DIRECTORY=flat\nexport ARCDOS_BENCH_ENTRIES={args.files}", flush=True)
                if server.wait() != 0:
                    raise RuntimeError("ArcDOS exited while serving the fixture")
            else:
                print(json.dumps(benchmark(args, port, server.pid, data, total, digest, startup), indent=2))
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
            log_thread.join(timeout=1)


def stop(_signal, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--web-dir", type=lambda p: Path(p).resolve(), default=Path("apps/web/build").resolve())
    parser.add_argument("--files", type=int, default=10_000)
    parser.add_argument("--samples", type=int, default=60)
    parser.add_argument("--clients", type=int, default=4)
    parser.add_argument("--transfer-mib", type=int, default=32)
    parser.add_argument("--index-timeout", type=int, default=600)
    parser.add_argument("--ai-url", help="Private embedding worker URL for an inference-enabled comparison")
    parser.add_argument("--ai-token-file", type=lambda p: Path(p).resolve())
    parser.add_argument("--serve", action="store_true", help="Serve an indexed synthetic fixture for browser measurements")
    args = parser.parse_args()
    if sys.platform != "linux" or not args.server.is_file():
        parser.error("provide a Linux ArcDOS server binary")
    if not (1 <= args.files <= 100_000 and 1 <= args.clients <= min(args.samples, 32)
            and 1 <= args.transfer_mib <= 128 and args.index_timeout >= 1):
        parser.error("invalid file count, transfer size, sample count, client count, or index timeout")
    if (args.ai_url is None) != (args.ai_token_file is None):
        parser.error("configure both --ai-url and --ai-token-file")
    signal.signal(signal.SIGTERM, stop)
    with tempfile.TemporaryDirectory(prefix="arcdos-bench-") as directory:
        try:
            run(args, Path(directory))
        except KeyboardInterrupt:
            if not args.serve:
                raise


if __name__ == "__main__":
    main()
