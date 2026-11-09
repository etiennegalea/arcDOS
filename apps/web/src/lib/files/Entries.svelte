<script lang="ts">
  import { tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { EntryKind, type Entry } from '../gen/arcdos/files/v1/files_pb';
  import Icon from './Icon.svelte';

  type ThumbnailLoader = (entry: Entry, signal: AbortSignal) => Promise<Blob | null>;
  type Props = {
    entries: readonly Entry[];
    view: 'list' | 'grid';
    selected: ReadonlySet<string>;
    favorites: ReadonlySet<string>;
    onOpen: (entry: Entry) => void;
    onSelect: (entry: Entry, event: MouseEvent | KeyboardEvent) => void;
    onFavorite: (entry: Entry) => void;
    onMoveDrop: (target: Entry, ids: string[]) => void;
    onUploadDrop: (files: File[], target?: Entry) => void;
    onLoadMore: () => void;
    hasMore: boolean;
    loadingMore: boolean;
    resetKey: string;
    readOnly?: boolean;
    autoLoad?: boolean;
    loadThumbnail?: ThumbnailLoader;
  };

  let {
    entries, view, selected, favorites, onOpen, onSelect, onFavorite,
    onMoveDrop, onUploadDrop, onLoadMore, hasMore, loadingMore, resetKey,
    readOnly = false, autoLoad = true, loadThumbnail
  }: Props = $props();

  const OVERSCAN = 4;
  const LIST_ROW = 46;
  const GRID_ROW = 146;
  const GRID_COLUMN = 170;
  const DRAG_FORMAT = 'application/x-arcdos-entry-ids';
  const MAX_THUMBNAILS = 24;
  const MAX_THUMBNAIL_BYTES = 24 * 1024 * 1024;
  const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' });
  const sizes = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  const rasterImage = /\.(?:png|jpe?g|gif|webp|avif|bmp|ico)$/i;

  let viewport: HTMLDivElement;
  let viewportWidth = $state(0);
  let viewportHeight = $state(0);
  let scrollTop = $state(0);
  let dropId = $state<string | null>(null);
  let draggedIds: string[] = [];
  let autoRequestedLength = -1;

  let columns = $derived(view === 'grid' ? Math.max(1, Math.floor(viewportWidth / GRID_COLUMN)) : 1);
  let rowHeight = $derived(view === 'grid' ? GRID_ROW : LIST_ROW);
  let rows = $derived(Math.ceil(entries.length / columns));
  let firstVisibleRow = $derived(Math.min(Math.max(0, rows - 1), Math.floor(scrollTop / rowHeight)));
  let afterVisibleRow = $derived(Math.ceil((scrollTop + viewportHeight) / rowHeight));
  let firstRow = $derived(Math.max(0, firstVisibleRow - OVERSCAN));
  let firstIndex = $derived(firstRow * columns);
  let rendered = $derived(entries.slice(firstIndex, (afterVisibleRow + OVERSCAN) * columns));

  $effect(() => {
    // Navigation and view changes reset the scroll position. Appended pages preserve it.
    void resetKey;
    void view;
    if (viewport) viewport.scrollTop = 0;
    scrollTop = 0;
    dropId = null;
    autoRequestedLength = -1;
  });

  $effect(() => {
    if (
      !autoLoad || !hasMore || loadingMore || !entries.length || !viewportHeight ||
      scrollTop + viewportHeight < (rows - 2) * rowHeight ||
      autoRequestedLength === entries.length
    ) return;
    // Keep the explicit button available if a page fails or the active filter adds no rows.
    autoRequestedLength = entries.length;
    untrack(onLoadMore);
  });

  function loadNextPage() {
    autoRequestedLength = entries.length;
    onLoadMore();
  }

  function entryIcon(entry: Entry): string {
    if (entry.kind === EntryKind.DIRECTORY) return 'folder';
    const name = entry.name;
    if (rasterImage.test(name)) return 'image';
    if (/\.pdf$/i.test(name)) return 'pdf';
    if (/\.(?:mp4|webm|mov|mkv|avi)$/i.test(name)) return 'video';
    if (/\.(?:mp3|wav|m4a|ogg|flac)$/i.test(name)) return 'audio';
    if (/\.(?:zip|rar|7z|gz|tar)$/i.test(name)) return 'archive';
    if (/\.(?:txt|md|csv|json|xml|log|ts|js|rs|html|css|py|toml|ya?ml)$/i.test(name)) return 'text';
    return 'file';
  }

  function sizeLabel(entry: Entry): string {
    if (entry.kind === EntryKind.DIRECTORY) return '—';
    const bytes = Number(entry.sizeBytes);
    const unit = bytes ? Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), sizes.length - 1) : 0;
    const count = bytes / 1024 ** unit;
    return `${count.toLocaleString(undefined, { maximumFractionDigits: unit ? 1 : 0 })} ${sizes[unit]}`;
  }

  function dateLabel(entry: Entry): string {
    if (!entry.modifiedAt) return '—';
    const date = new Date(Number(entry.modifiedAt.seconds) * 1000);
    return Number.isNaN(date.valueOf()) ? '—' : dateFormat.format(date);
  }

  function openOrSelect(entry: Entry, event: MouseEvent) {
    if (event.ctrlKey || event.metaKey || event.shiftKey) onSelect(entry, event);
    else onOpen(entry);
  }

  async function navigateKey(index: number, entry: Entry, event: KeyboardEvent) {
    if (event.key === ' ') {
      event.preventDefault();
      onSelect(entry, event);
      return;
    }
    const jump = Math.max(columns, Math.floor(viewportHeight / rowHeight) * columns);
    let target: number;
    switch (event.key) {
      case 'ArrowDown': target = index + columns; break;
      case 'ArrowUp': target = index - columns; break;
      case 'ArrowRight': if (view === 'list') return; target = index + 1; break;
      case 'ArrowLeft': if (view === 'list') return; target = index - 1; break;
      case 'PageDown': target = index + jump; break;
      case 'PageUp': target = index - jump; break;
      case 'Home': target = 0; break;
      case 'End': target = entries.length - 1; break;
      default: return;
    }
    event.preventDefault();
    target = Math.min(entries.length - 1, Math.max(0, target));
    if (event.shiftKey && target !== index) onSelect(entries[target], event);
    const top = Math.floor(target / columns) * rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + rowHeight > viewport.scrollTop + viewportHeight) {
      viewport.scrollTop = top + rowHeight - viewportHeight;
    }
    scrollTop = viewport.scrollTop;
    await tick();
    viewport?.querySelector<HTMLButtonElement>(`[data-entry-index="${target}"] .entry-name`)
      ?.focus({ preventScroll: true });
  }

  function beginDrag(entry: Entry, event: DragEvent) {
    if (!event.dataTransfer || readOnly || entry.trashed) {
      event.preventDefault();
      return;
    }
    draggedIds = selected.has(entry.entryId) ? [...selected] : [entry.entryId];
    event.dataTransfer.setData(DRAG_FORMAT, 'entries');
    event.dataTransfer.effectAllowed = 'move';
  }

  function endDrag() {
    draggedIds = [];
    dropId = null;
  }

  function folderTarget(event: DragEvent): Entry | undefined {
    if (!(event.target instanceof Element)) return;
    const row = event.target.closest<HTMLElement>('[data-entry-index]');
    const index = row ? Number(row.dataset.entryIndex) : -1;
    const entry = entries[index];
    return entry?.kind === EntryKind.DIRECTORY && !entry.trashed ? entry : undefined;
  }

  function dragOver(event: DragEvent) {
    if (!event.dataTransfer) return;
    const externalFiles = event.dataTransfer.types.includes('Files');
    const target = folderTarget(event);
    const move = target && draggedIds.length > 0 && !draggedIds.includes(target.entryId);
    if (externalFiles || move) event.preventDefault();
    if (readOnly || (!externalFiles && !move)) {
      dropId = null;
      return;
    }
    event.dataTransfer.dropEffect = externalFiles ? 'copy' : 'move';
    dropId = target?.entryId ?? '';
  }

  function dragLeave(event: DragEvent) {
    if (!(event.relatedTarget instanceof Node) || !viewport.contains(event.relatedTarget)) {
      dropId = null;
    }
  }

  function drop(event: DragEvent) {
    const transfer = event.dataTransfer;
    const target = folderTarget(event);
    const externalFiles = transfer?.types.includes('Files');
    if (externalFiles || draggedIds.length) event.preventDefault();
    if (!readOnly && transfer?.files.length) onUploadDrop(Array.from(transfer.files), target);
    else if (!readOnly && target && draggedIds.length && !draggedIds.includes(target.entryId)) {
      onMoveDrop(target, draggedIds);
    }
    endDrag();
  }

  type Thumbnail = { url: string; bytes: number };
  const thumbnails = new SvelteMap<string, Thumbnail | null>();
  const thumbnailRequests = new SvelteMap<string, AbortController>();
  let thumbnailBytes = 0;
  let desiredThumbnails = new SvelteMap<string, Entry>();
  let activeLoader: ThumbnailLoader | undefined;

  function thumbnailKey(entry: Entry) {
    return `${entry.entryId}:${entry.revision}`;
  }

  function forgetThumbnail(key: string) {
    const thumbnail = thumbnails.get(key);
    if (thumbnail) {
      URL.revokeObjectURL(thumbnail.url);
      thumbnailBytes -= thumbnail.bytes;
    }
    thumbnails.delete(key);
  }

  function rememberThumbnail(key: string, blob: Blob | null) {
    const bytes = blob?.size ?? 0;
    for (const cached of thumbnails.keys()) {
      if (thumbnails.size < MAX_THUMBNAILS && thumbnailBytes + bytes <= MAX_THUMBNAIL_BYTES) break;
      if (!desiredThumbnails.has(cached)) forgetThumbnail(cached);
    }
    if (thumbnails.size >= MAX_THUMBNAILS) return;
    if (blob && thumbnailBytes + bytes <= MAX_THUMBNAIL_BYTES) {
      thumbnails.set(key, { url: URL.createObjectURL(blob), bytes });
      thumbnailBytes += bytes;
    } else {
      thumbnails.set(key, null);
    }
  }

  function scheduleThumbnails() {
    const loader = activeLoader;
    if (!loader) return;
    for (const [key, entry] of desiredThumbnails) {
      if (thumbnailRequests.size >= 3) break;
      if (thumbnails.has(key) || thumbnailRequests.has(key)) continue;
      const controller = new AbortController();
      thumbnailRequests.set(key, controller);
      void loader(entry, controller.signal).then(
        (blob) => { if (!controller.signal.aborted) rememberThumbnail(key, blob); },
        () => { if (!controller.signal.aborted) rememberThumbnail(key, null); }
      ).finally(() => {
        thumbnailRequests.delete(key);
        scheduleThumbnails();
      });
    }
  }

  function clearThumbnails() {
    desiredThumbnails.clear();
    for (const controller of thumbnailRequests.values()) controller.abort();
    for (const key of thumbnails.keys()) forgetThumbnail(key);
  }

  $effect(() => {
    const loader = loadThumbnail;
    const visible = view === 'grid' && viewportHeight > 0
      ? entries.slice(firstVisibleRow * columns, afterVisibleRow * columns)
      : [];
    untrack(() => {
      if (activeLoader !== loader) clearThumbnails();
      activeLoader = loader;
      desiredThumbnails = new SvelteMap(visible
        .filter((entry) => !entry.trashed && entry.kind === EntryKind.FILE && rasterImage.test(entry.name))
        .slice(0, MAX_THUMBNAILS)
        .map((entry) => [thumbnailKey(entry), entry]));
      for (const [key, controller] of thumbnailRequests) {
        if (!desiredThumbnails.has(key)) controller.abort();
      }
      scheduleThumbnails();
    });
  });

  $effect(() => () => {
    activeLoader = undefined;
    clearThumbnails();
  });
</script>

<div class="entries">
  {#if view === 'list' && entries.length}
    <div class="entry-header" aria-hidden="true">
      <span></span>
      <span>Name</span>
      <span class="modified">Modified</span>
      <span class="size">Size</span>
      <span></span>
    </div>
  {/if}

  <div
    class="entry-viewport"
    class:drop-zone={dropId === ''}
    role="region"
    aria-label="File entries"
    bind:this={viewport}
    bind:clientHeight={viewportHeight}
    bind:clientWidth={viewportWidth}
    onscroll={() => { scrollTop = viewport.scrollTop; }}
    ondragover={dragOver}
    ondragleave={dragLeave}
    ondrop={drop}
  >
    <div class="entry-space" style:height={`${rows * rowHeight}px`}>
      <ul
        class:grid-view={view === 'grid'}
        class:list-view={view === 'list'}
        class="entry-window"
        aria-label="Files and folders"
        style:top={`${firstRow * rowHeight}px`}
        style:--entry-columns={columns}
        style:--entry-height={`${rowHeight}px`}
      >
        {#each rendered as entry, offset (entry.entryId)}
          {@const index = firstIndex + offset}
          {@const marked = selected.has(entry.entryId)}
          {@const favorite = favorites.has(entry.entryId)}
          {@const icon = entryIcon(entry)}
          {@const thumbnail = view === 'grid' ? thumbnails.get(thumbnailKey(entry)) : null}
          <li
            class="entry-item"
            class:marked
            class:drop-target={dropId === entry.entryId}
            data-entry-index={index}
            aria-posinset={index + 1}
            aria-setsize={hasMore ? -1 : entries.length}
          >
            <button
              type="button"
              class="entry-select"
              role="checkbox"
              aria-checked={marked}
              aria-label={`Select ${entry.name}`}
              onclick={(event) => onSelect(entry, event)}
            >
              <span class="selection-mark">{#if marked}<Icon name="check" size={12} />{/if}</span>
            </button>
            <button
              type="button"
              class="entry-name"
              aria-label={entry.name}
              title={entry.name}
              draggable={!readOnly && !entry.trashed}
              ondragstart={(event) => beginDrag(entry, event)}
              ondragend={endDrag}
              onclick={(event) => openOrSelect(entry, event)}
              onkeydown={(event) => { void navigateKey(index, entry, event); }}
            >
              <span class="entry-icon" class:folder-icon={icon === 'folder'} class:image-icon={icon === 'image'}>
                {#if thumbnail}
                  <img src={thumbnail.url} alt="" draggable="false" decoding="async" />
                {:else}
                  <Icon name={icon} size={view === 'grid' ? 33 : 18} />
                {/if}
              </span>
              <span class="entry-title">{entry.name}</span>
            </button>
            <span class="modified" title={dateLabel(entry)}>{dateLabel(entry)}</span>
            <span class="size">{sizeLabel(entry)}</span>
            <button
              type="button"
              class="entry-star"
              aria-label={favorite ? `Unfavorite ${entry.name}` : `Favorite ${entry.name}`}
              aria-pressed={favorite}
              title={favorite ? 'Remove from favorites' : 'Add to favorites'}
              onclick={() => onFavorite(entry)}
            >
              <Icon name="star" size={16} filled={favorite} />
            </button>
          </li>
        {/each}
      </ul>
    </div>
  </div>

  {#if hasMore || loadingMore}
    <div class="entry-pagination">
      <button type="button" disabled={loadingMore} onclick={loadNextPage}>
        {loadingMore ? 'Loading…' : 'Load more'}
      </button>
    </div>
  {/if}
</div>

<style>
  .entries {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    color: var(--drive-text, #24302a);
    font-size: 0.85rem;
    container-type: inline-size;
  }

  .entry-header, .list-view .entry-item {
    display: grid;
    grid-template-columns: 2.25rem minmax(0, 1fr) 8.5rem 5rem 2.25rem;
    align-items: center;
    column-gap: 0.65rem;
    padding-inline: 0.4rem 0.6rem;
  }

  .entry-header {
    overflow: hidden;
    scrollbar-gutter: stable;
    min-height: 2rem;
    border-bottom: 1px solid var(--drive-border, #dfe7de);
    color: var(--drive-muted, #5a6860);
    font-size: 0.73rem;
    font-weight: 650;
  }

  .entry-header .size, .list-view .size { text-align: right; }

  .entry-viewport {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-gutter: stable;
    overscroll-behavior: contain;
  }

  .entry-viewport.drop-zone {
    background: var(--drive-selected, #e8f2eb);
    box-shadow: inset 0 0 0 2px var(--drive-accent, #3b6e52);
  }

  .entry-space { position: relative; min-height: 100%; }

  .entry-window {
    position: absolute;
    inset-inline: 0;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .entry-item {
    box-sizing: border-box;
    min-width: 0;
    min-height: 0;
    border: 1px solid transparent;
  }

  .list-view .entry-item {
    height: var(--entry-height);
    border-bottom-color: var(--drive-border, #e4eae3);
  }

  .entry-item:hover { background: var(--drive-hover, #f1f5ef); }
  .entry-item.marked { background: var(--drive-selected, #e8f2eb); }
  .entry-item.drop-target { border-color: var(--drive-accent, #3b6e52); background: var(--drive-selected, #e8f2eb); }

  .entry-item button, .entry-pagination button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    box-sizing: border-box;
    border: 1px solid transparent;
    border-radius: 0.4rem;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }

  .entry-item button:focus-visible, .entry-pagination button:focus-visible {
    outline: 2px solid var(--drive-accent, #3b6e52);
    outline-offset: 2px;
  }

  .entry-select, .entry-star {
    width: 2.1rem;
    height: 2.1rem;
    padding: 0;
    color: var(--drive-muted, #65736a);
  }

  .entry-select:hover, .entry-star:hover { background: var(--drive-control-hover, #dce9df); }
  .entry-star[aria-pressed='true'] { color: var(--drive-star, #8f6120); }

  .selection-mark {
    display: grid;
    place-items: center;
    width: 0.95rem;
    height: 0.95rem;
    box-sizing: border-box;
    border: 1px solid var(--drive-control-border, #87978a);
    border-radius: 0.24rem;
    background: var(--drive-surface, #fff);
  }

  .entry-select[aria-checked='true'] .selection-mark {
    border-color: var(--drive-accent, #3b6e52);
    background: var(--drive-accent, #3b6e52);
    color: #fff;
  }

  .entry-item .entry-name {
    min-width: 0;
    height: 2.1rem;
    justify-content: flex-start;
    gap: 0.55rem;
    padding: 0;
    font-weight: 550;
    text-align: left;
  }

  .entry-icon {
    display: grid;
    flex: none;
    place-items: center;
    color: var(--drive-muted, #596b5f);
  }

  .entry-icon.folder-icon { color: var(--drive-accent, #3b6e52); }
  .entry-icon.image-icon { color: var(--drive-image, #4e678a); }

  .entry-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .modified, .size { white-space: nowrap; }
  .entry-item .modified, .entry-item .size {
    color: var(--drive-muted, #5a6860);
    font-size: 0.77rem;
    font-variant-numeric: tabular-nums;
  }

  .grid-view {
    display: grid;
    grid-template-columns: repeat(var(--entry-columns), minmax(0, 1fr));
    grid-auto-rows: var(--entry-height);
    padding-inline: 0.2rem;
  }

  .grid-view .entry-item {
    position: relative;
    margin: 0.25rem;
    border-color: var(--drive-border, #dfe7de);
    border-radius: 0.62rem;
    background: var(--drive-surface, #fff);
  }

  .grid-view .entry-item:hover { background: var(--drive-hover, #f1f5ef); }
  .grid-view .entry-item.marked, .grid-view .entry-item.drop-target {
    border-color: var(--drive-accent, #3b6e52);
    background: var(--drive-selected, #e8f2eb);
  }

  .grid-view .entry-name {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    width: 100%;
    height: 100%;
    gap: 0.4rem;
    padding: 0.45rem 0.55rem 0.7rem;
  }

  .grid-view .entry-icon {
    width: 100%;
    height: 5.5rem;
    overflow: hidden;
    border-radius: 0.35rem;
    background: var(--drive-tile, #edf2ec);
  }

  .grid-view .entry-icon img {
    width: 100%;
    height: 5.5rem;
    object-fit: cover;
  }

  .grid-view .entry-title { padding-inline: 0.14rem; }
  .grid-view .modified, .grid-view .size { display: none; }
  .grid-view .entry-select, .grid-view .entry-star {
    position: absolute;
    z-index: 1;
    top: 0.34rem;
    background: var(--drive-surface, #fff);
  }
  .grid-view .entry-select:hover, .grid-view .entry-star:hover {
    background: var(--drive-control-hover, #dce9df);
  }
  .grid-view .entry-select { left: 0.36rem; }
  .grid-view .entry-star { right: 0.36rem; }

  .entry-pagination {
    display: flex;
    justify-content: center;
    padding: 0.45rem;
    border-top: 1px solid var(--drive-border, #dfe7de);
  }

  .entry-pagination button {
    min-height: 1.95rem;
    padding: 0.2rem 0.85rem;
    border-color: var(--drive-border, #dfe7de);
    background: var(--drive-surface, #fff);
  }

  .entry-pagination button:hover:enabled { background: var(--drive-hover, #f1f5ef); }
  .entry-pagination button:disabled { cursor: default; opacity: 0.6; }

  @container (max-width: 38rem) {
    .entry-header, .list-view .entry-item {
      grid-template-columns: 2.25rem minmax(0, 1fr) 5rem 2.25rem;
    }
    .modified { display: none; }
  }

  @container (max-width: 24rem) {
    .entry-header, .list-view .entry-item {
      grid-template-columns: 2.25rem minmax(0, 1fr) 2.25rem;
      gap: 0.4rem;
    }
    .size { display: none; }
  }
</style>
