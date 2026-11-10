"""Private embedding service. Run with python -m worker.server."""

import hmac
import json
import os
import re
import signal
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Protocol

from .embeddings import OnnxEmbedder


MAX_BODY_BYTES = 256 * 1024
MAX_BATCH = 16
CONNECTION_LIMIT = 4
SOCKET_TIMEOUT = 5


class Embedder(Protocol):
    dimensions: int

    def embed(self, texts: list[str]) -> list[list[float]]: ...


class RequestError(Exception):
    def __init__(self, status: HTTPStatus, message: str):
        super().__init__(message)
        self.status = status


class WorkerServer(ThreadingHTTPServer):
    request_queue_size = CONNECTION_LIMIT

    def __init__(self, address: tuple[str, int], token: str, model: str, embedder: Embedder):
        self.authorization = f"Bearer {token}".encode("ascii")
        self.model = model
        self.embedder = embedder
        self.inference = threading.Lock()
        self.connections = threading.BoundedSemaphore(CONNECTION_LIMIT)
        super().__init__(address, WorkerHandler)

    def get_request(self):
        request, address = super().get_request()
        request.settimeout(SOCKET_TIMEOUT)
        return request, address

    def process_request(self, request, address):
        if not self.connections.acquire(blocking=False):
            body = b'{"error":"worker busy"}'
            try:
                request.sendall(
                    b"HTTP/1.0 429 Too Many Requests\r\nConnection: close\r\n"
                    b"Content-Type: application/json\r\nCache-Control: no-store\r\n"
                    + f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
                    + body
                )
            except OSError:
                pass
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.connections.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.connections.release()


class WorkerHandler(BaseHTTPRequestHandler):
    server: WorkerServer

    def do_GET(self):
        if not self.authorize():
            return
        if self.path != "/healthz":
            self.respond(HTTPStatus.NOT_FOUND, {"error": "unknown endpoint"})
            return
        self.respond(
            HTTPStatus.OK,
            {
                "ready": True,
                "embedding": {
                    "model": self.server.model,
                    "dimensions": self.server.embedder.dimensions,
                },
            },
        )

    def do_POST(self):
        if not self.authorize():
            return
        if self.path != "/v1/embed":
            self.respond(HTTPStatus.NOT_FOUND, {"error": "unknown endpoint"})
            return
        try:
            texts = self.read_texts()
        except RequestError as error:
            self.respond(error.status, {"error": str(error)})
            return
        if not self.server.inference.acquire(blocking=False):
            self.respond(HTTPStatus.TOO_MANY_REQUESTS, {"error": "worker busy"})
            return
        try:
            vectors = self.server.embedder.embed(texts)
        except Exception:
            self.log_message("embedding inference failed")
            self.respond(HTTPStatus.SERVICE_UNAVAILABLE, {"error": "embedding failed"})
        else:
            self.respond(HTTPStatus.OK, {"model": self.server.model, "vectors": vectors})
        finally:
            self.server.inference.release()

    def authorize(self) -> bool:
        headers = self.headers.get_all("Authorization", [])
        if len(headers) == 1 and hmac.compare_digest(
            headers[0].encode("latin-1"), self.server.authorization
        ):
            return True
        self.respond(HTTPStatus.UNAUTHORIZED, {"error": "unauthorized"})
        return False

    def read_texts(self) -> list[str]:
        sizes = self.headers.get_all("Content-Length", [])
        if (
            self.headers.get("Transfer-Encoding") is not None
            or len(sizes) != 1
            or not re.fullmatch(r"[0-9]{1,10}", sizes[0])
        ):
            raise RequestError(HTTPStatus.BAD_REQUEST, "expected a single content length")
        size = int(sizes[0])
        if size > MAX_BODY_BYTES:
            raise RequestError(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "request too large")
        if self.headers.get_content_type() != "application/json":
            raise RequestError(HTTPStatus.UNSUPPORTED_MEDIA_TYPE, "expected application/json")

        body = bytearray()
        deadline = time.monotonic() + SOCKET_TIMEOUT
        while len(body) < size:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RequestError(HTTPStatus.REQUEST_TIMEOUT, "request body timed out")
            self.connection.settimeout(remaining)
            try:
                chunk = self.rfile.read1(size - len(body))
            except TimeoutError as error:
                raise RequestError(HTTPStatus.REQUEST_TIMEOUT, "request body timed out") from error
            finally:
                self.connection.settimeout(SOCKET_TIMEOUT)
            if not chunk:
                raise RequestError(HTTPStatus.BAD_REQUEST, "incomplete request body")
            body.extend(chunk)

        try:
            payload = json.loads(body.decode("utf-8"))
            if isinstance(payload, dict) and payload.keys() == {"texts"}:
                texts = payload["texts"]
                if (
                    isinstance(texts, list)
                    and 1 <= len(texts) <= MAX_BATCH
                    and all(isinstance(text, str) and text.strip() for text in texts)
                ):
                    for text in texts:
                        text.encode("utf-8")
                    return texts
        except (UnicodeError, ValueError, RecursionError):
            pass
        raise RequestError(HTTPStatus.BAD_REQUEST, "expected 1 to 16 nonempty UTF-8 texts")

    def respond(self, status: HTTPStatus, payload: dict):
        body = json.dumps(payload, allow_nan=False, separators=(",", ":")).encode("utf-8")
        self.close_connection = True
        try:
            self.send_response_only(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
        except OSError:
            pass

    def log_request(self, code="-", size="-"):
        # Keep client-controlled request targets and content out of logs.
        pass

    def log_error(self, format, *args):
        # The stdlib's protocol error messages may contain client-supplied values.
        super().log_error("worker request failed")


def load_token() -> str:
    token_file = os.environ.get("ARCDOS_AI_TOKEN_FILE")
    token = os.environ.get("ARCDOS_AI_TOKEN")
    if bool(token_file) == bool(token):
        raise ValueError("configure exactly one worker token source")
    if token_file:
        with Path(token_file).open("rb") as secret:
            contents = secret.read(259)
        if len(contents) > 258:
            raise ValueError("worker token file is too large")
        token = contents.decode("ascii").strip()
    if not re.fullmatch(r"[A-Za-z0-9_-]{32,256}", token):
        raise ValueError("worker token must contain 32 to 256 URL-safe characters")
    return token


def stop(_signum, _frame):
    raise KeyboardInterrupt


def main():
    token = load_token()
    model = os.environ["ARCDOS_AI_MODEL"]
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,127}", model):
        raise ValueError("invalid embedding model identity")
    host = os.environ.get("ARCDOS_AI_BIND_HOST", "127.0.0.1")
    if host not in {"127.0.0.1", "0.0.0.0"}:
        raise ValueError("worker bind host must be loopback or a private container network")
    port = int(os.environ.get("ARCDOS_AI_BIND_PORT", "8081"))
    threads = int(os.environ.get("ARCDOS_AI_THREADS", "2"))
    if not 1 <= port <= 65535 or not 1 <= threads <= 8:
        raise ValueError("invalid worker port or thread count")
    model_path = Path(os.environ["ARCDOS_AI_MODEL_PATH"])
    tokenizer_path = Path(os.environ["ARCDOS_AI_TOKENIZER_PATH"])
    embedder = OnnxEmbedder(model_path, tokenizer_path, threads)
    signal.signal(signal.SIGTERM, stop)
    with WorkerServer((host, port), token, model, embedder) as server:
        print(f"ArcDOS embedding worker listening on {host}:{port}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
