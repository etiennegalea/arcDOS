import http.client
import json
import os
import tempfile
import threading
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from .server import CONNECTION_LIMIT, MAX_BATCH, MAX_BODY_BYTES, WorkerServer, load_token


TOKEN = "synthetic-worker-token-for-tests-only"
MODEL = "synthetic-model:v1"


class FakeEmbedder:
    dimensions = 2

    def __init__(self):
        self.calls = []

    def embed(self, texts):
        self.calls.append(texts)
        if texts == ["fail"]:
            raise ValueError("test failure")
        return [[1.0, 0.0] for _ in texts]


class WorkerTest(unittest.TestCase):
    def setUp(self):
        self.embedder = FakeEmbedder()
        self.server = WorkerServer(("127.0.0.1", 0), TOKEN, MODEL, self.embedder)
        self.addCleanup(self.server.server_close)
        thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.01})
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(self.server.shutdown)

    def request(self, method, path, body=None, token=TOKEN, headers=None):
        request_headers = {"Content-Type": "application/json"}
        if token is not None:
            request_headers["Authorization"] = f"Bearer {token}"
        request_headers.update(headers or {})
        with closing(http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)) as connection:
            connection.request(method, path, body=body, headers=request_headers)
            with connection.getresponse() as response:
                return response.status, json.loads(response.read())

    def embed(self, texts):
        return self.request("POST", "/v1/embed", body=json.dumps({"texts": texts}))

    def test_health_and_embedding_contract(self):
        self.assertEqual(
            self.request("GET", "/healthz"),
            (200, {"ready": True, "embedding": {"model": MODEL, "dimensions": 2}}),
        )
        self.assertEqual(
            self.embed(["hello", "ġurnata"]),
            (200, {"model": MODEL, "vectors": [[1.0, 0.0], [1.0, 0.0]]}),
        )
        self.assertEqual(self.embedder.calls, [["hello", "ġurnata"]])

    def test_authentication_precedes_inference(self):
        self.assertEqual(self.request("GET", "/healthz", token=None)[0], 401)
        self.assertEqual(
            self.request("POST", "/v1/embed", body=b'{"texts":["hi"]}', token="é")[0],
            401,
        )
        self.assertEqual(self.embedder.calls, [])

    def test_rejects_invalid_or_oversized_requests(self):
        for texts in [[], [" "], ["\ud800"], [1], ["x"] * (MAX_BATCH + 1)]:
            with self.subTest(texts=texts):
                self.assertEqual(self.embed(texts)[0], 400)
        self.assertEqual(self.request("POST", "/v1/embed", body=b"\xff")[0], 400)
        self.assertEqual(
            self.request(
                "POST",
                "/v1/embed",
                body=b"",
                headers={"Content-Length": str(MAX_BODY_BYTES + 1)},
            )[0],
            413,
        )
        self.assertEqual(
            self.request("POST", "/v1/embed", body=b"", headers={"Transfer-Encoding": "chunked"})[0],
            400,
        )
        self.assertEqual(self.embedder.calls, [])

    def test_busy_worker_stays_healthy_and_recovers(self):
        with self.server.inference:
            self.assertEqual(self.embed(["waiting"])[0], 429)
            self.assertEqual(self.request("GET", "/healthz")[0], 200)
        self.assertEqual(self.embedder.calls, [])
        self.assertEqual(self.embed(["fail"])[0], 503)
        self.assertEqual(self.embed(["retry"])[0], 200)

    def test_connection_limit(self):
        for _ in range(CONNECTION_LIMIT):
            self.server.connections.acquire()
            self.addCleanup(self.server.connections.release)
        self.assertEqual(self.request("GET", "/healthz"), (429, {"error": "worker busy"}))


class TokenTest(unittest.TestCase):
    def test_token_file_with_newline(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "worker-token"
            path.write_bytes(f"{TOKEN}\r\n".encode("ascii"))
            with patch.dict(os.environ, {"ARCDOS_AI_TOKEN_FILE": str(path)}, clear=True):
                self.assertEqual(load_token(), TOKEN)

    def test_requires_one_strong_token_source(self):
        for env in [
            {},
            {"ARCDOS_AI_TOKEN": "short"},
            {"ARCDOS_AI_TOKEN": TOKEN, "ARCDOS_AI_TOKEN_FILE": "ignored"},
        ]:
            with self.subTest(env=env), patch.dict(os.environ, env, clear=True):
                with self.assertRaises(ValueError):
                    load_token()
