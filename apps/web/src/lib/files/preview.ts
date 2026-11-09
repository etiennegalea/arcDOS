import { EntryKind, type Entry } from '../gen/arcdos/files/v1/files_pb';

export type ContentLoader = (id: string, options?: RequestInit) => Promise<Response>;

const MIB = 1024 * 1024;
const TEXT_LIMIT = 128 * 1024;
const THUMBNAIL_LIMIT = MIB;

type MediaKind = 'image' | 'pdf' | 'audio' | 'video';
type Media = { kind: MediaKind; type: string; limit: number };

export type PreviewContent =
  | { kind: 'text'; text: string; truncated: boolean }
  | { kind: MediaKind; blob: Blob }
  | { kind: 'unavailable'; message: string };

const images: Record<string, string> = {
  avif: 'image/avif',
  bmp: 'image/bmp',
  gif: 'image/gif',
  jpeg: 'image/jpeg',
  jpg: 'image/jpeg',
  png: 'image/png',
  webp: 'image/webp'
};

const audio: Record<string, string> = {
  aac: 'audio/aac',
  flac: 'audio/flac',
  m4a: 'audio/mp4',
  mp3: 'audio/mpeg',
  oga: 'audio/ogg',
  ogg: 'audio/ogg',
  opus: 'audio/ogg',
  wav: 'audio/wav'
};

const video: Record<string, string> = {
  m4v: 'video/mp4',
  mov: 'video/quicktime',
  mp4: 'video/mp4',
  ogv: 'video/ogg',
  webm: 'video/webm'
};

const textExtensions = new Set(
  'c cfg cpp cs css csv go h hpp html ini java js json jsonl jsx log md mjs ps1 py rb rs sh sql svg svelte toml ts tsv tsx txt xml yaml yml'.split(' ')
);
const textNames = new Set(['dockerfile', 'license', 'makefile', 'readme']);

function extension(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot < 0 ? '' : name.slice(dot + 1).toLowerCase();
}

function lookup(types: Record<string, string>, name: string): string | undefined {
  return Object.hasOwn(types, name) ? types[name] : undefined;
}

export function previewMedia(name: string): Media | undefined {
  const ext = extension(name);
  const imageType = lookup(images, ext);
  if (imageType) return { kind: 'image', type: imageType, limit: 16 * MIB };
  if (ext === 'pdf') return { kind: 'pdf', type: 'application/pdf', limit: 24 * MIB };
  const audioType = lookup(audio, ext);
  if (audioType) return { kind: 'audio', type: audioType, limit: 32 * MIB };
  const videoType = lookup(video, ext);
  if (videoType) return { kind: 'video', type: videoType, limit: 64 * MIB };
}

class PreviewLimit extends Error {
  constructor(limit: number) {
    super(`Preview is limited to ${limit / MIB} MiB for this file type.`);
  }
}

async function boundedBody(
  response: Response,
  limit: number,
  partial: boolean,
  type: string,
  signal: AbortSignal
): Promise<{ blob: Blob; clipped: boolean }> {
  const reader = response.body?.getReader();
  if (!reader) return { blob: new Blob([], { type }), clipped: false };
  const cancel = () => { void reader.cancel().catch(() => undefined); };
  signal.addEventListener('abort', cancel, { once: true });

  const parts: BlobPart[] = [];
  let size = 0;
  try {
    if (!partial && Number(response.headers.get('content-length')) > limit) {
      throw new PreviewLimit(limit);
    }
    for (;;) {
      signal.throwIfAborted();
      const { done, value } = await reader.read();
      signal.throwIfAborted();
      if (done) return { blob: new Blob(parts, { type }), clipped: false };
      const remaining = limit - size;
      if (value.byteLength > remaining) {
        if (!partial) throw new PreviewLimit(limit);
        if (remaining > 0) parts.push(value.slice(0, remaining));
        return { blob: new Blob(parts, { type }), clipped: true };
      }
      if (value.byteLength === 0) continue;
      parts.push(value);
      size += value.byteLength;
    }
  } finally {
    signal.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

async function fetchContent(
  entry: Entry,
  load: ContentLoader,
  options: RequestInit,
  signal: AbortSignal
): Promise<Response> {
  const response = await load(entry.entryId, { ...options, signal });
  if (!response.ok) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(response.status === 401
      ? 'Reconnect to load this preview.'
      : `Could not load this preview (HTTP ${response.status}).`);
  }
  return response;
}

export async function loadPreview(
  entry: Entry,
  load: ContentLoader,
  signal: AbortSignal
): Promise<PreviewContent> {
  if (entry.kind !== EntryKind.FILE || entry.trashed) {
    return { kind: 'unavailable', message: 'Preview is available for files in the drive.' };
  }

  const media = previewMedia(entry.name);
  if (media) {
    if (entry.sizeBytes > BigInt(media.limit)) throw new PreviewLimit(media.limit);
    const response = await fetchContent(entry, load, {}, signal);
    const { blob } = await boundedBody(response, media.limit, false, media.type, signal);
    if (media.kind === 'pdf' && await blob.slice(0, 5).text() !== '%PDF-') {
      throw new Error('This file does not have a valid PDF header.');
    }
    return { kind: media.kind, blob };
  }

  if (!textExtensions.has(extension(entry.name)) && !textNames.has(entry.name.toLowerCase())) {
    return { kind: 'unavailable', message: 'This file type can be downloaded.' };
  }
  const headers = entry.sizeBytes > BigInt(TEXT_LIMIT)
    ? { Range: `bytes=0-${TEXT_LIMIT - 1}` }
    : undefined;
  const response = await fetchContent(entry, load, { headers }, signal);
  const { blob, clipped } = await boundedBody(response, TEXT_LIMIT, true, 'text/plain', signal);
  const range = response.headers.get('content-range')?.match(/^bytes 0-(\d+)\/(\d+)$/);
  const truncated = clipped || (response.status === 206 && (range
    ? BigInt(range[1]) + 1n < BigInt(range[2])
    : BigInt(blob.size) < entry.sizeBytes));
  const buffer = await blob.arrayBuffer();
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(buffer, { stream: truncated });
  } catch {
    return { kind: 'unavailable', message: 'Text preview requires UTF-8 content.' };
  }
  if (text.includes('\0')) {
    return { kind: 'unavailable', message: 'This file contains binary data.' };
  }
  return { kind: 'text', text, truncated };
}

/** Small raster images for the visible directory window. */
export async function loadThumbnail(
  entry: Entry,
  load: ContentLoader,
  signal: AbortSignal
): Promise<Blob | null> {
  const type = lookup(images, extension(entry.name));
  if (!type || entry.kind !== EntryKind.FILE || entry.trashed || entry.sizeBytes > BigInt(THUMBNAIL_LIMIT)) {
    return null;
  }

  try {
    const response = await fetchContent(entry, load, {}, signal);
    const { blob } = await boundedBody(response, THUMBNAIL_LIMIT, false, type, signal);
    return blob.size > 0 ? blob : null;
  } catch (error) {
    if (error instanceof PreviewLimit) return null;
    throw error;
  }
}
