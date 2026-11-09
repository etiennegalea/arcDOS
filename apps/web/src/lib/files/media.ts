import type { ContentLoader } from './preview';

const SCOPE = '/files/';
const WORKER_PATH = `${SCOPE}media-worker.js`;
const MEDIA_PATH = `${SCOPE}_media/`;
const HANDSHAKE_MS = 3_000;
const forwardedHeaders = ['accept-ranges', 'content-length', 'content-range', 'etag'];
const byteRange = /^bytes=(?:\d{1,20}-\d{0,20}|-\d{1,20})$/i;

function supportsStreamTransfer(): boolean {
  const { port1, port2 } = new MessageChannel();
  try {
    const stream = new ReadableStream({ start: (controller) => controller.close() });
    port1.postMessage(stream, [stream]);
    return true;
  } catch {
    return false;
  } finally {
    port1.close();
    port2.close();
  }
}

function waitForWorker(
  container: ServiceWorkerContainer,
  script: string,
  signal: AbortSignal
): Promise<ServiceWorker | null> {
  if (signal.aborted) return Promise.resolve(null);
  if (container.controller?.scriptURL === script) return Promise.resolve(container.controller);

  return new Promise((resolve) => {
    const finish = (worker: ServiceWorker | null) => {
      clearTimeout(timer);
      signal.removeEventListener('abort', cancel);
      container.removeEventListener('controllerchange', check);
      resolve(worker);
    };
    const check = () => {
      const worker = container.controller;
      if (worker?.scriptURL === script) finish(worker);
    };
    const cancel = () => finish(null);
    const timer = setTimeout(cancel, HANDSHAKE_MS);
    signal.addEventListener('abort', cancel, { once: true });
    container.addEventListener('controllerchange', check);
    void container.register(script, { scope: SCOPE }).then(check, cancel);
  });
}

async function relay(
  port: MessagePort,
  data: { range?: unknown; ifRange?: unknown },
  id: string,
  type: string,
  load: ContentLoader,
  signal: AbortSignal
) {
  const request = new AbortController();
  port.onmessage = (event) => {
    if (event.data === 'cancel') request.abort();
  };
  const requestSignal = AbortSignal.any([signal, request.signal]);
  const headers = new Headers();
  if (typeof data.range === 'string' && byteRange.test(data.range)) {
    headers.set('Range', data.range);
    if (typeof data.ifRange === 'string' && /^[\x20-\x7e]{1,256}$/.test(data.ifRange)) {
      headers.set('If-Range', data.ifRange);
    }
  }

  let response: Response | undefined;
  try {
    response = await load(id, { headers, signal: requestSignal });
    requestSignal.throwIfAborted();
    const status = response.status;
    const streaming = status === 200 || status === 206;
    const body = streaming ? response.body : null;
    const outgoing = forwardedHeaders.flatMap((name) => {
      if (!streaming && !(status === 416 && name === 'content-range')) return [];
      const value = response?.headers.get(name);
      return value === null || value === undefined ? [] : [[name, value]];
    });
    port.postMessage({ status, type, headers: outgoing, body }, body ? [body] : []);
    if (!body) await response.body?.cancel().catch(() => undefined);
  } catch {
    await response?.body?.cancel().catch(() => undefined);
    port.postMessage({ status: 503 });
  } finally {
    port.close();
  }
}

/** A transient path relayed only through the page that owns this preview. */
export async function openMediaStream(
  id: string,
  type: string,
  load: ContentLoader,
  signal: AbortSignal
): Promise<string | null> {
  if (
    !globalThis.isSecureContext
    || !('serviceWorker' in navigator)
    || typeof AbortSignal.any !== 'function'
    || !location.pathname.startsWith(SCOPE)
    || !supportsStreamTransfer()
  ) return null;

  const container = navigator.serviceWorker;
  const script = new URL(WORKER_PATH, location.origin).href;
  const worker = await waitForWorker(container, script, signal);
  if (!worker || signal.aborted) return null;

  const key = crypto.randomUUID().replaceAll('-', '');
  container.addEventListener('message', (event: MessageEvent) => {
    const data = event.data;
    if (!event.isTrusted || event.source !== worker || data?.type !== 'arcdos-media' || data.key !== key) return;
    const port = event.ports[0];
    if (port) void relay(port, data, id, type, load, signal);
  }, { signal });
  return MEDIA_PATH + key;
}
