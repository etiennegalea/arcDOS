const mediaPath = /^\/files\/_media\/([a-f0-9]{32})$/;
const contentTypes = /^(?:audio\/(?:aac|flac|mp4|mpeg|ogg|wav)|video\/(?:mp4|quicktime|ogg|webm))$/;
const forwardedHeaders = new Set(['accept-ranges', 'content-length', 'content-range', 'etag']);
const replyTimeoutMs = 12_000;

globalThis.addEventListener('install', (event) => event.waitUntil(globalThis.skipWaiting()));
globalThis.addEventListener('activate', (event) => event.waitUntil(globalThis.clients.claim()));

globalThis.addEventListener('fetch', (event) => {
  const { request } = event;
  if (request.method !== 'GET' || (request.destination !== 'audio' && request.destination !== 'video')) return;
  const url = new globalThis.URL(request.url);
  const match = mediaPath.exec(url.pathname);
  if (url.origin === globalThis.location.origin && !url.search && match) {
    event.respondWith(mediaResponse(event, match[1]));
  }
});

function failure(status) {
  return new globalThis.Response(null, { status, headers: { 'Cache-Control': 'no-store' } });
}

function makeResponse(reply) {
  const status = reply?.status;
  if (status !== 200 && status !== 206 && status !== 416) {
    return failure([401, 403, 404].includes(status) ? status : 503);
  }

  const headers = new globalThis.Headers({
    'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff'
  });
  for (const [name, value] of reply.headers) {
    if (forwardedHeaders.has(name) && (status !== 416 || name === 'content-range')) headers.set(name, value);
  }
  if (status === 416) return new globalThis.Response(null, { status, headers });
  if (!(reply.body instanceof globalThis.ReadableStream)) return failure(503);
  if (!contentTypes.test(reply.type)) {
    void reply.body.cancel().catch(() => undefined);
    return failure(503);
  }
  headers.set('Content-Type', reply.type);
  return new globalThis.Response(reply.body, { status, headers });
}

async function mediaResponse(event, key) {
  const client = event.clientId && await globalThis.clients.get(event.clientId);
  const { request } = event;
  if (!client || client.type !== 'window' || request.signal.aborted) return failure(404);

  return new Promise((resolve) => {
    const { port1, port2 } = new globalThis.MessageChannel();
    const finish = (response) => {
      globalThis.clearTimeout(timer);
      request.signal.removeEventListener('abort', cancel);
      port1.close();
      resolve(response);
    };
    const cancel = () => {
      port1.postMessage('cancel');
      finish(failure(503));
    };
    const timer = globalThis.setTimeout(cancel, replyTimeoutMs);
    request.signal.addEventListener('abort', cancel, { once: true });
    port1.onmessage = ({ data }) => {
      try {
        finish(makeResponse(data));
      } catch {
        if (data?.body instanceof globalThis.ReadableStream) {
          void data.body.cancel().catch(() => undefined);
        }
        finish(failure(503));
      }
    };
    port1.onmessageerror = cancel;
    try {
      client.postMessage({
        type: 'arcdos-media',
        key,
        range: request.headers.get('Range'),
        ifRange: request.headers.get('If-Range')
      }, [port2]);
    } catch {
      port2.close();
      finish(failure(503));
    }
  });
}
