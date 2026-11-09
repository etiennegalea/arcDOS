import { Code, ConnectError, createClient, type Client } from '@connectrpc/connect';
import { createConnectTransport } from '@connectrpc/connect-web';
import { FileService, TransferService, type Entry } from '../gen/arcdos/files/v1/files_pb';

export interface FileClient {
  files: Client<typeof FileService>;
  transfers: Client<typeof TransferService>;
  content(id: string, options?: RequestInit): Promise<Response>;
  patch(id: string, bytes: Blob, offset: number, signal: AbortSignal): Promise<Response>;
}

export function createFileClient(origin: string, token: string): FileClient {
  const authorization = `Bearer ${token}`;
  const transport = createConnectTransport({
    baseUrl: origin,
    useBinaryFormat: true,
    interceptors: [(next) => (request) => {
      request.header.set('Authorization', authorization);
      return next(request);
    }],
    fetch: (request, options) => fetch(request, { ...options, redirect: 'error' })
  });

  function fetchFile(path: string, options: RequestInit = {}) {
    const headers = new Headers(options.headers);
    headers.set('Authorization', authorization);
    return fetch(new URL(path, origin), { ...options, headers, cache: 'no-store', redirect: 'error' });
  }

  return {
    files: createClient(FileService, transport),
    transfers: createClient(TransferService, transport),
    content: (id, options) => fetchFile(`/api/files/v1/${encodeURIComponent(id)}/content`, options),
    patch: (id, bytes, offset, signal) => fetchFile(`/api/files/v1/uploads/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/octet-stream', 'Upload-Offset': String(offset) },
      body: bytes,
      signal
    })
  };
}

export const rpcOptions = (signal?: AbortSignal) => ({ signal, timeoutMs: 12_000 });

export function requireEntry(entry: Entry | undefined): Entry {
  if (!entry) throw new Error('The server returned no file metadata.');
  return entry;
}

export function fileError(error: unknown): string {
  const failure = ConnectError.from(error);
  switch (failure.code) {
    case Code.Aborted:
      return 'The files changed. Refresh and try again.';
    case Code.AlreadyExists:
      return 'An item with that name already exists.';
    case Code.Unauthenticated:
      return 'The development token was rejected. Check the token and try again.';
    case Code.Unavailable:
    case Code.DeadlineExceeded:
      return 'The file service did not respond. Try again.';
    default:
      return failure.rawMessage || 'The file operation failed.';
  }
}
