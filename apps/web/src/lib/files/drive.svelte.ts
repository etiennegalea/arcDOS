import { Code, ConnectError } from '@connectrpc/connect';
import { EntryKind, type Entry } from '../gen/arcdos/files/v1/files_pb';
import { createFileClient, fileError, requireEntry, rpcOptions, type FileClient } from './client';

export type Section = 'files' | 'recent' | 'favorites' | 'trash';
export type Layout = 'list' | 'grid';
export type Sort = 'default' | 'name' | 'newest' | 'largest';
export interface Location { section: Section; folderId: string }

const PAGE_SIZE = 120;
const PREFERENCES = 'arcdos.files.preferences.v1';
const MAX_RECENT = 60;
const MAX_FAVORITES = 250;
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

export function locationFrom(url: { searchParams: Pick<URLSearchParams, 'get'> }): Location {
  const view = url.searchParams.get('view');
  if (view === 'recent' || view === 'favorites' || view === 'trash') {
    return { section: view, folderId: '' };
  }
  return { section: 'files', folderId: url.searchParams.get('folder') ?? '' };
}

export function viewEntries(entries: Entry[], filter: string, sort: Sort, section: Section): Entry[] {
  const search = filter.trim().toLocaleLowerCase();
  const filtered = search ? entries.filter((entry) => entry.name.toLocaleLowerCase().includes(search)) : entries;
  if (sort === 'default' && section === 'recent') return filtered;
  return [...filtered].sort((left, right) => {
    if (left.kind !== right.kind) return left.kind === EntryKind.DIRECTORY ? -1 : 1;
    if (sort === 'newest') {
      const seconds = compare(right.modifiedAt?.seconds ?? 0n, left.modifiedAt?.seconds ?? 0n);
      const nanos = (right.modifiedAt?.nanos ?? 0) - (left.modifiedAt?.nanos ?? 0);
      if (seconds || nanos) return seconds || nanos;
    }
    if (sort === 'largest') {
      const size = compare(right.sizeBytes, left.sizeBytes);
      if (size) return size;
    }
    return collator.compare(left.name, right.name) || collator.compare(left.entryId, right.entryId);
  });
}

function compare(a: bigint, b: bigint) {
  return a === b ? 0 : a < b ? -1 : 1;
}

export class Drive {
  client = $state.raw<FileClient | null>(null);
  location = $state.raw<Location>({ section: 'files', folderId: '' });
  entries = $state.raw<Entry[]>([]);
  folder = $state.raw<Entry | null>(null);
  breadcrumbs = $state.raw<Entry[]>([]);
  next = $state('');
  loading = $state(false);
  loadingMore = $state(false);
  connecting = $state(false);
  error = $state('');
  moreError = $state('');
  changesAvailable = $state(false);
  layout = $state<Layout>('list');
  favorites = $state.raw<string[]>([]);
  recent = $state.raw<string[]>([]);

  #navigation?: AbortController;
  #connect?: AbortController;
  #subscription?: AbortController;

  initialize() {
    try {
      const saved = JSON.parse(localStorage.getItem(PREFERENCES) ?? 'null');
      if (!saved || typeof saved !== 'object') return;
      if (saved.layout === 'grid') this.layout = 'grid';
      this.favorites = preferenceIds(saved.favorites, MAX_FAVORITES);
      this.recent = preferenceIds(saved.recent, MAX_RECENT);
    } catch {
      // A browser can block storage. The workspace still works for this visit.
    }
  }

  setLayout(layout: Layout) {
    this.layout = layout;
    this.#save();
  }

  toggleFavorite(entry: Entry) {
    if (entry.trashed) return;
    this.favorites = this.favorites.includes(entry.entryId)
      ? this.favorites.filter((id) => id !== entry.entryId)
      : [entry.entryId, ...this.favorites].slice(0, MAX_FAVORITES);
    this.#save();
  }

  remember(entry: Entry) {
    if (entry.kind !== EntryKind.FILE || entry.trashed) return;
    this.recent = [entry.entryId, ...this.recent.filter((id) => id !== entry.entryId)].slice(0, MAX_RECENT);
    this.#save();
  }

  #save() {
    try {
      localStorage.setItem(PREFERENCES, JSON.stringify({
        layout: this.layout, favorites: this.favorites, recent: this.recent
      }));
    } catch {
      // Preferences are optional when storage is disabled.
    }
  }

  async connect(token: string): Promise<boolean> {
    this.#connect?.abort();
    const request = new AbortController();
    this.#connect = request;
    this.connecting = true;
    this.error = '';
    try {
      const client = createFileClient(window.location.origin, token);
      requireEntry((await client.files.getEntry({}, rpcOptions(request.signal))).entry);
      if (request.signal.aborted) return false;
      this.client = client;
      this.#watch(client);
      return true;
    } catch (error) {
      if (!request.signal.aborted) {
        this.error = ConnectError.from(error).code === Code.Unimplemented
          ? 'The file API is disabled. Start ArcDOS with its data directory and development token configured.'
          : fileError(error);
      }
      return false;
    } finally {
      if (!request.signal.aborted) this.connecting = false;
    }
  }

  disconnect() {
    this.#connect?.abort();
    this.#navigation?.abort();
    this.#subscription?.abort();
    this.client = null;
    this.folder = null;
    this.breadcrumbs = [];
    this.entries = [];
    this.next = '';
    this.error = '';
    this.moreError = '';
    this.connecting = false;
    this.loading = false;
    this.loadingMore = false;
    this.changesAvailable = false;
  }

  async open(location: Location, refresh = false) {
    const client = this.client;
    if (!client) return;
    this.#navigation?.abort();
    const request = new AbortController();
    this.#navigation = request;
    this.location = location;
    this.loading = true;
    this.loadingMore = false;
    this.error = '';
    this.moreError = '';
    this.changesAvailable = false;
    if (!refresh) {
      this.entries = [];
      this.folder = null;
      this.breadcrumbs = [];
      this.next = '';
    }
    const options = rpcOptions(request.signal);
    try {
      if (location.section === 'files') {
        const [trail, page] = await Promise.all([
          getTrail(client, location.folderId, request.signal),
          client.files.listEntries({ parentId: location.folderId, pageSize: PAGE_SIZE }, options)
        ]);
        if (request.signal.aborted) return;
        this.folder = trail[trail.length - 1];
        this.breadcrumbs = trail;
        this.entries = page.entries;
        this.next = page.nextPageToken;
      } else if (location.section === 'trash') {
        const page = await client.files.listTrash({ pageSize: PAGE_SIZE }, options);
        if (request.signal.aborted) return;
        this.entries = page.entries;
        this.next = page.nextPageToken;
      } else {
        const ids = location.section === 'favorites' ? this.favorites : this.recent;
        const entries = await getKnownEntries(client, ids, request.signal);
        if (request.signal.aborted) return;
        this.entries = entries;
        this.next = '';
      }
    } catch (error) {
      if (!request.signal.aborted) this.error = fileError(error);
    } finally {
      if (!request.signal.aborted) this.loading = false;
    }
  }

  refresh() {
    return this.open(this.location, true);
  }

  async loadMore() {
    const client = this.client;
    const navigation = this.#navigation;
    const token = this.next;
    if (!client || !navigation || !token || this.loading || this.loadingMore) return;
    this.moreError = '';
    this.loadingMore = true;
    const options = rpcOptions(navigation.signal);
    try {
      const page = this.location.section === 'trash'
        ? await client.files.listTrash({ pageSize: PAGE_SIZE, pageToken: token }, options)
        : await client.files.listEntries({
          parentId: this.location.folderId, pageSize: PAGE_SIZE, pageToken: token
        }, options);
      if (navigation.signal.aborted) return;
      if (page.nextPageToken === token) throw new Error('The server returned the same page cursor.');
      this.entries = [...this.entries, ...page.entries];
      this.next = page.nextPageToken;
    } catch (error) {
      if (!navigation.signal.aborted) this.moreError = fileError(error);
    } finally {
      if (!navigation.signal.aborted) this.loadingMore = false;
    }
  }

  #watch(client: FileClient) {
    this.#subscription?.abort();
    const controller = new AbortController();
    this.#subscription = controller;
    const { signal } = controller;
    let sequence = 0n;
    let caughtUp = false;
    let timer: number;

    const poll = async () => {
      if (signal.aborted) return;
      try {
        if (document.visibilityState === 'visible') {
          const result = await client.files.listChanges({ afterSequence: sequence, pageSize: 500 }, rpcOptions(signal));
          if (signal.aborted) return;
          if (caughtUp && result.changes.length) {
            const folder = this.folder;
            const location = this.location;
            if (!folder || location.section !== 'files') {
              this.changesAvailable = true;
            } else {
              let changed = false;
              try {
                const response = await client.files.getEntry({ entryId: folder.entryId }, rpcOptions(signal));
                changed = response.entry?.revision !== folder.revision;
              } catch (error) {
                const code = ConnectError.from(error).code;
                if (code !== Code.NotFound && code !== Code.Aborted) throw error;
                changed = true;
              }
              if (changed && !signal.aborted && this.folder === folder && this.location === location) {
                this.changesAvailable = true;
              }
            }
          }
          if (!signal.aborted) {
            sequence = result.nextSequence;
            caughtUp ||= result.changes.length < 500;
          }
        }
      } catch {
        // Keep the last acknowledged sequence so a failed poll can catch up.
      } finally {
        if (!signal.aborted) timer = window.setTimeout(poll, 4000);
      }
    };
    signal.addEventListener('abort', () => window.clearTimeout(timer), { once: true });
    void poll();
  }
}

async function getTrail(client: FileClient, id: string, signal: AbortSignal): Promise<Entry[]> {
  const trail: Entry[] = [];
  let next = id;
  while (trail.length < 64) {
    const entry = requireEntry((await client.files.getEntry({ entryId: next }, rpcOptions(signal))).entry);
    if (trail.some((parent) => parent.entryId === entry.entryId) || entry.trashed || entry.kind !== EntryKind.DIRECTORY) {
      throw new Error('This folder is no longer available.');
    }
    trail.unshift(entry);
    if (!entry.parentId) return trail;
    next = entry.parentId;
  }
  throw new Error('The folder path is too deep to display.');
}

async function getKnownEntries(client: FileClient, ids: string[], signal: AbortSignal): Promise<Entry[]> {
  const entries: Entry[] = [];
  for (let offset = 0; offset < ids.length; offset += 8) {
    const batch = await Promise.all(ids.slice(offset, offset + 8).map(async (entryId) => {
      try {
        return (await client.files.getEntry({ entryId }, rpcOptions(signal))).entry;
      } catch (error) {
        const code = ConnectError.from(error).code;
        if (code === Code.NotFound || code === Code.Aborted) return undefined;
        throw error;
      }
    }));
    entries.push(...batch.filter((entry): entry is Entry => entry !== undefined && !entry.trashed));
  }
  return entries;
}

function preferenceIds(value: unknown, limit: number): string[] {
  return Array.isArray(value)
    ? Array.from(new Set(value.filter((id): id is string => typeof id === 'string' && id.length > 0 && id.length <= 128))).slice(0, limit)
    : [];
}
