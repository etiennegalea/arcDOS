# Local semantic search

Enable semantic ranking on an existing Linux file server configured for [Authentik](authentik.md) or a [synthetic development instance](README.md#enable-the-linux-file-api). ArcDOS sends bounded UTF-8 chunks and search queries to a separate local CPU worker. The worker requires a shared bearer token and the deployment below gives it a private network and read-only access to model artifacts. Search stays usable during worker outages.

## Prepare the worker

Place `onnx/model.onnx` and `tokenizer.json` from the pinned all-MiniLM-L6-v2 release in `/srv/arcdos/models/minilm`. The model is Apache-2.0 licensed. Acquire and verify the model files before deployment. Give UID 10001 read access to both files. The worker loads these local artifacts at startup. [Pinned model artifacts and license](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/tree/1110a243fdf4706b3f48f1d95db1a4f5529b4d41) · [ONNX Runtime Python API](https://onnxruntime.ai/docs/api/python/api_summary.html)

```sh
cd /srv/arcdos/models/minilm
sha256sum --check <<'EOF'
6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452  onnx/model.onnx
be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037  tokenizer.json
EOF
```

From the repository root, generate a worker credential separately from the Authentik and development credentials. Store it outside the shared data and static web directories.

```sh
sudo install -d -m 0755 /etc/arcdos
openssl rand -hex 32 | sudo sh -c 'umask 077; cat > /etc/arcdos/ai-token'
sudo chown 10001:10001 /etc/arcdos/ai-token
sudo chmod 0400 /etc/arcdos/ai-token

docker build -f deploy/Dockerfile.ai-worker -t arcdos-ai:local .
docker network create --internal arcdos-ai
docker run -d --name arcdos-ai-worker --network arcdos-ai --network-alias ai-worker \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges --restart unless-stopped \
  --cpus 2 --memory 768m --pids-limit 64 \
  --mount type=bind,source=/srv/arcdos/models/minilm,target=/models,readonly \
  --mount type=bind,source=/etc/arcdos/ai-token,target=/run/arcdos-ai-token,readonly \
  -e ARCDOS_AI_BIND_HOST=0.0.0.0 \
  -e ARCDOS_AI_TOKEN_FILE=/run/arcdos-ai-token \
  -e ARCDOS_AI_MODEL_PATH=/models/onnx/model.onnx \
  -e ARCDOS_AI_TOKENIZER_PATH=/models/tokenizer.json \
  -e ARCDOS_AI_MODEL=all-MiniLM-L6-v2@1110a243fdf4706b3f48f1d95db1a4f5529b4d41:mean-l2-v1 \
  arcdos-ai:local
```

Keep the worker port unpublished. The worker's only runtime mounts are the model directory and token file. [Docker internal networks](https://docs.docker.com/reference/cli/docker/network/create/#network-internal-mode---internal) · [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/)

## Enable ranking on the file server

Add the following arguments to the existing ArcDOS server container command. Use the newly built `arcdos:local` image.

```sh
--mount type=bind,source=/etc/arcdos/ai-token,target=/run/arcdos-ai-token,readonly \
-e ARCDOS_AI_TOKEN_FILE=/run/arcdos-ai-token \
-e ARCDOS_AI_WORKER_URL=http://ai-worker:8081
```

Attach the server container to the worker's network in addition to its normal network. Replace `arcdos` with the server container name.

```sh
docker network connect arcdos-ai arcdos
```

ArcDOS retries indexing in the background. After indexing a synthetic text file, search for a related phrase and check that the file appears. Run `docker stop arcdos-ai-worker`, then check that a literal keyword still finds the file and that it opens. Run `docker start arcdos-ai-worker` to resume semantic indexing.

The server accepts the private `ai-worker` service name or `127.0.0.1` for a same-host worker outside Docker. Point both processes at the same token file. Leave both server settings unset to use keyword search alone. Treat `/data/state/semantic.db` and its WAL as private derived file data. Change `ARCDOS_AI_MODEL` when changing the model or tokenizer artifacts so the server can rebuild embeddings in the correct space.
