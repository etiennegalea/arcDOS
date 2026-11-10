import { Code, ConnectError, createClient, type Client } from '@connectrpc/connect';
import { createConnectTransport } from '@connectrpc/connect-web';
import { FileService, TransferService, type Entry } from '../gen/arcdos/files/v1/files_pb';
import { SearchService } from '../gen/arcdos/search/v1/search_pb';

export interface FileClient {
  files: Client<typeof FileService>;
  transfers: Client<typeof TransferService>;
  search: Client<typeof SearchService>;
  content(id: string, options?: RequestInit): Promise<Response>;
  patch(id: string, bytes: Blob, offset: number, signal: AbortSignal): Promise<Response>;
}

export type FileAuth =
  | { mode: 'development'; token: string }
  | { mode: 'oidc'; csrfToken: string };

export function createFileClient(origin: string, auth: FileAuth, onUnauthenticated: () => void): FileClient {
  const credentials: RequestCredentials = auth.mode === 'oidc' ? 'same-origin' : 'omit';
  const options: RequestInit = { credentials, mode: 'same-origin', cache: 'no-store', redirect: 'error' };
  const transport = createConnectTransport({
    baseUrl: origin,
    useBinaryFormat: true,
    interceptors: [(next) => async (request) => {
      authenticate(request.header, true);
      try {
        return await next(request);
      } catch (error) {
        if (ConnectError.from(error).code === Code.Unauthenticated) onUnauthenticated();
        throw error;
      }
    }],
    fetch: (request, init) => fetch(request, { ...init, ...options })
  });

  function authenticate(headers: Headers, csrf: boolean) {
    headers.delete('Authorization');
    headers.delete('X-CSRF-Token');
    if (auth.mode === 'development') headers.set('Authorization', `Bearer ${auth.token}`);
    else if (csrf) headers.set('X-CSRF-Token', auth.csrfToken);
  }

  async function fetchFile(path: string, init: RequestInit = {}) {
    const headers = new Headers(init.headers);
    const method = init.method?.toUpperCase() ?? 'GET';
    authenticate(headers, method !== 'GET' && method !== 'HEAD');
    const response = await fetch(new URL(path, origin), { ...init, ...options, headers });
    if (response.status === 401) onUnauthenticated();
    return response;
  }

  return {
    files: createClient(FileService, transport),
    transfers: createClient(TransferService, transport),
    search: createClient(SearchService, transport),
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
      return 'Sign in to continue.';
    case Code.Unavailable:
    case Code.DeadlineExceeded:
      return 'The file service did not respond. Try again.';
    default:
      return failure.rawMessage || 'The file operation failed.';
  }
}
