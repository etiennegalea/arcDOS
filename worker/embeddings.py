"""Bounded CPU inference for a local all-MiniLM-L6-v2 ONNX export."""

import os
from pathlib import Path


class OnnxEmbedder:
    dimensions = 384
    max_tokens = 256

    def __init__(self, model_path: Path, tokenizer_path: Path, threads: int):
        os.environ["TOKENIZERS_PARALLELISM"] = "false"
        import numpy as np
        import onnxruntime as ort
        from tokenizers import Tokenizer

        self.np = np
        self.tokenizer = Tokenizer.from_file(str(tokenizer_path))
        pad_id = self.tokenizer.token_to_id("[PAD]")
        if pad_id is None:
            raise ValueError("expected a MiniLM tokenizer with a [PAD] token")
        self.tokenizer.enable_padding(pad_id=pad_id)
        self.tokenizer.enable_truncation(max_length=self.max_tokens)

        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.add_session_config_entry("session.intra_op.allow_spinning", "0")
        self.session = ort.InferenceSession(
            str(model_path),
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
        self.inputs = {item.name: item.type for item in self.session.get_inputs()}
        required = {"input_ids", "attention_mask"}
        if not (
            required <= self.inputs.keys() <= required | {"token_type_ids"}
            and all(dtype == "tensor(int64)" for dtype in self.inputs.values())
        ):
            raise ValueError("expected MiniLM int64 token inputs")
        self.embed(["ArcDOS readiness check"])

    def embed(self, texts: list[str]) -> list[list[float]]:
        np = self.np
        encoded = self.tokenizer.encode_batch(texts)
        attention_mask = np.array([item.attention_mask for item in encoded], dtype=np.int64)
        inputs = {
            "input_ids": np.array([item.ids for item in encoded], dtype=np.int64),
            "attention_mask": attention_mask,
        }
        if "token_type_ids" in self.inputs:
            inputs["token_type_ids"] = np.array(
                [item.type_ids for item in encoded], dtype=np.int64
            )
        (hidden,) = self.session.run(["last_hidden_state"], inputs)
        if hidden.shape != (*attention_mask.shape, self.dimensions):
            raise ValueError("unexpected MiniLM output shape")

        mask = attention_mask[..., None]
        pooled = (hidden * mask).sum(axis=1) / mask.sum(axis=1)
        norms = np.linalg.norm(pooled, axis=1, keepdims=True)
        if not np.isfinite(norms).all() or (norms == 0).any():
            raise ValueError("invalid MiniLM output")
        return (pooled / norms).tolist()
