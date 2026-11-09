import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { create } from '@bufbuild/protobuf';
import { expect, test } from '@playwright/test';
import ts from 'typescript';
import { EntryKind, EntrySchema } from '../src/lib/gen/arcdos/files/v1/files_pb';
import { loadPreview, loadThumbnail, type ContentLoader } from '../src/lib/files/preview';

const TEXT_LIMIT = 128 * 1024;
const MIB = 1024 * 1024;

function entry(name: string, sizeBytes: bigint) {
  return create(EntrySchema, { entryId: 'synthetic-file', name, sizeBytes, kind: EntryKind.FILE });
}

test('requests a bounded text range and preserves complete UTF-8 characters', async () => {
  const bytes = new Uint8Array(TEXT_LIMIT).fill(65);
  bytes[TEXT_LIMIT - 1] = 0xe2;
  const controller = new AbortController();
  const load: ContentLoader = async (id, options) => {
    expect(id).toBe('synthetic-file');
    expect(new Headers(options?.headers).get('range')).toBe('bytes=0-131071');
    expect(options?.signal).toBe(controller.signal);
    return new Response(bytes, {
      status: 206,
      headers: { 'Content-Range': 'bytes 0-131071/131074' }
    });
  };

  const preview = await loadPreview(entry('README.md', 131074n), load, controller.signal);
  expect(preview).toEqual({ kind: 'text', text: 'A'.repeat(TEXT_LIMIT - 1), truncated: true });
});

test('limits text reads when the server ignores the range', async () => {
  let cancelled = false;
  const stream = new ReadableStream({
    start(writer) {
      writer.enqueue(new Uint8Array(TEXT_LIMIT + 1024).fill(65));
    },
    cancel() {
      cancelled = true;
    }
  });
  const preview = await loadPreview(
    entry('server.log', BigInt(TEXT_LIMIT + 1024)),
    async () => new Response(stream),
    new AbortController().signal
  );
  expect(preview).toEqual({ kind: 'text', text: 'A'.repeat(TEXT_LIMIT), truncated: true });
  expect(cancelled).toBe(true);
});

test('uses text rendering for markup and checks the PDF signature before embedding', async () => {
  const html = '<script>globalThis.untrusted = true</script>';
  const signal = new AbortController().signal;
  const load = async () => new Response(html);
  await expect(loadPreview(entry('example.html', BigInt(html.length)), load, signal))
    .resolves.toEqual({ kind: 'text', text: html, truncated: false });
  await expect(loadPreview(entry('example.pdf', BigInt(html.length)), load, signal))
    .rejects.toThrow('valid PDF header');
  const result = await loadPreview(
    entry('example.pdf', 8n),
    async () => new Response('%PDF-1.4', { headers: { 'Content-Type': 'application/octet-stream' } }),
    signal
  );
  expect(result.kind).toBe('pdf');
  if ('blob' in result) expect(result.blob.type).toBe('application/pdf');
});

test('does not fetch oversized media and rejects oversized thumbnail responses', async () => {
  const signal = new AbortController().signal;
  const overLimit = BigInt(64 * MIB + 1);
  let fetched = false;
  await expect(loadPreview(entry('video.mp4', overLimit), async () => {
    fetched = true;
    return new Response();
  }, signal)).rejects.toThrow('64 MiB');
  expect(fetched).toBe(false);

  await expect(loadThumbnail(
    entry('image.png', 100n),
    async () => new Response('small', { headers: { 'Content-Length': String(MIB + 1) } }),
    signal
  )).resolves.toBeNull();
});

test('cancels a pending body read when the preview closes', async () => {
  const controller = new AbortController();
  let readStarted = () => {};
  const reading = new Promise<void>((resolve) => { readStarted = resolve; });
  let cancelled = false;
  const stream = new ReadableStream({
    pull() {
      readStarted();
    },
    cancel() {
      cancelled = true;
    }
  }, { highWaterMark: 0 });
  const result = loadPreview(
    entry('example.txt', 20n),
    async () => new Response(stream),
    controller.signal
  );
  await reading;
  controller.abort();
  await expect(result).rejects.toThrow();
  expect(cancelled).toBe(true);
});

test('streams native audio ranges through its owning page', async ({ page }) => {
  const [source, worker] = await Promise.all([
    readFile(new URL('../src/lib/files/media.ts', import.meta.url), 'utf8'),
    readFile(new URL('../static/files/media-worker.js', import.meta.url), 'utf8')
  ]);
  const client = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
  }).outputText;
  const dataBytes = 80 * MIB;
  const totalBytes = dataBytes + 44;
  const wave = Buffer.alloc(44);
  wave.write('RIFF', 0);
  wave.writeUInt32LE(totalBytes - 8, 4);
  wave.write('WAVEfmt ', 8);
  wave.writeUInt32LE(16, 16);
  wave.writeUInt16LE(1, 20);
  wave.writeUInt16LE(1, 22);
  wave.writeUInt32LE(8_000, 24);
  wave.writeUInt32LE(8_000, 28);
  wave.writeUInt16LE(1, 32);
  wave.writeUInt16LE(8, 34);
  wave.write('data', 36);
  wave.writeUInt32LE(dataBytes, 40);
  const pcm = Buffer.alloc(32 * 1024, 128);
  const ranges: string[] = [];
  const authorizations: string[] = [];
  const server = createServer((request, response) => {
    const path = request.url;
    if (path === '/files/') {
      response.setHeader('Content-Type', 'text/html');
      response.end('<!doctype html><title>ArcDOS media test</title>');
    } else if (path === '/files/media-worker.js' || path === '/test-media-client.js') {
      response.setHeader('Content-Type', 'text/javascript');
      response.end(path === '/files/media-worker.js' ? worker : client);
    } else if (path === '/test-content') {
      authorizations.push(request.headers.authorization ?? '');
      const range = request.headers.range ?? '';
      ranges.push(range);
      const bounds = /^bytes=(\d+)-(\d*)$/.exec(range);
      let position = bounds ? Number(bounds[1]) : 0;
      const end = bounds?.[2] ? Math.min(Number(bounds[2]), totalBytes - 1) : totalBytes - 1;
      if (position > end) {
        response.writeHead(416, { 'Content-Range': `bytes */${totalBytes}` }).end();
        return;
      }
      response.writeHead(bounds ? 206 : 200, {
        'Accept-Ranges': 'bytes',
        'Content-Type': 'application/octet-stream',
        'Content-Length': end - position + 1,
        ...(bounds ? { 'Content-Range': `bytes ${position}-${end}/${totalBytes}` } : {})
      });
      const timer = setInterval(() => {
        if (position > end) {
          response.end();
          clearInterval(timer);
        } else if (!response.writableNeedDrain) {
          const chunk = position < wave.length
            ? wave.subarray(position, end + 1)
            : pcm.subarray(0, end - position + 1);
          response.write(chunk);
          position += chunk.length;
        }
      }, 10);
      response.once('close', () => clearInterval(timer));
    } else {
      response.writeHead(404).end();
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });

  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Test server has no TCP port.');
    await page.goto(`http://127.0.0.1:${address.port}/files/`);
    const result = await page.evaluate(async () => {
      const modulePath = '/test-media-client.js';
      const { openMediaStream } = await import(modulePath) as typeof import('../src/lib/files/media');
      const controller = new AbortController();
      const url = await openMediaStream('sample', 'audio/wav', (_id, options) => {
        const headers = new Headers(options?.headers);
        headers.set('Authorization', 'Bearer synthetic-test-credential');
        return fetch('/test-content', { ...options, headers });
      }, controller.signal);
      if (!url) throw new Error('Could not activate the media worker.');
      const audio = document.createElement('audio');
      const waitForMedia = (eventName: string) => new Promise<void>((resolve, reject) => {
        audio.addEventListener(eventName, () => resolve(), { once: true });
        audio.addEventListener('error', () => reject(new Error(audio.error?.message)), { once: true });
      });
      const metadata = waitForMedia('loadedmetadata');
      audio.preload = 'metadata';
      audio.src = url;
      document.body.append(audio);
      await metadata;
      const duration = audio.duration;
      const seeked = waitForMedia('seeked');
      audio.currentTime = duration * 0.75;
      await seeked;
      controller.abort();
      audio.removeAttribute('src');
      audio.load();
      audio.remove();
      return { url, duration };
    });
    expect(result.url).toMatch(/^\/files\/_media\/[a-f0-9]{32}$/);
    expect(result.duration).toBe(dataBytes / 8_000);
    expect(ranges[0]).toBe('bytes=0-');
    expect(ranges.some((range) => Number(/^bytes=(\d+)/.exec(range)?.[1]) > MIB)).toBe(true);
    expect(authorizations.every((header) => header === 'Bearer synthetic-test-credential')).toBe(true);
  } finally {
    await page.goto('about:blank');
    await new Promise<void>((resolve) => {
      server.close(() => resolve());
      server.closeAllConnections();
    });
  }
});
