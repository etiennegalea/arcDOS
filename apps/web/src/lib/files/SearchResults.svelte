<script lang="ts">
  import { tick } from 'svelte';
  import { EntryKind, type Entry } from '../gen/arcdos/files/v1/files_pb';
  import type { SearchHit } from '../gen/arcdos/search/v1/search_pb';
  import { fileError, rpcOptions, type FileClient } from './client';
  import Icon from './Icon.svelte';

  let { client, query, onOpen, onClear }: {
    client: FileClient;
    query: string;
    onOpen: (entry: Entry) => void;
    onClear: () => void;
  } = $props();

  let hits = $state.raw<SearchHit[]>([]);
  let hasMore = $state(false);
  let searching = $state(true);
  let error = $state('');
  let editing = $state.raw<SearchHit | null>(null);
  let tagInput = $state('');
  let tagInputElement = $state<HTMLTextAreaElement>();
  let saving = $state(false);
  let tagError = $state('');
  let tagRequest: AbortController | undefined;
  let tagButton: HTMLButtonElement | undefined;

  $effect(() => {
    const activeClient = client;
    const term = query;
    const request = new AbortController();
    hits = [];
    hasMore = false;
    searching = true;
    error = '';
    editing = null;
    saving = false;
    const timer = window.setTimeout(() => {
      void load(activeClient, term, request.signal);
    }, 225);
    return () => {
      window.clearTimeout(timer);
      request.abort();
      tagRequest?.abort();
    };
  });

  async function load(activeClient: FileClient, term: string, signal: AbortSignal) {
    try {
      const response = await activeClient.search.searchFiles({ query: term }, rpcOptions(signal));
      if (signal.aborted) return;
      hits = response.hits.filter((hit) => hit.entry?.entryId && !hit.entry.trashed);
      hasMore = response.hasMore;
    } catch (failure) {
      if (!signal.aborted) error = fileError(failure);
    } finally {
      if (!signal.aborted) searching = false;
    }
  }

  async function editTags(hit: SearchHit, button: HTMLButtonElement) {
    editing = hit;
    tagButton = button;
    tagInput = hit.tags.join('\n');
    tagError = '';
    await tick();
    tagInputElement?.focus();
  }

  async function closeEditor() {
    editing = null;
    await tick();
    tagButton?.focus();
  }

  async function saveTags(event: SubmitEvent) {
    event.preventDefault();
    const entry = editing?.entry;
    if (!entry || saving) return;
    const tags = [...new Set(tagInput.split('\n').map((tag) => tag.trim()).filter(Boolean))];
    if (tags.length > 24 || tags.some((tag) => [...tag].length > 48)) {
      tagError = 'Use up to 24 tags of 48 characters each.';
      return;
    }

    const request = new AbortController();
    tagRequest = request;
    saving = true;
    tagError = '';
    try {
      const response = await client.search.setTags({
        entryId: entry.entryId, expectedRevision: entry.revision, tags
      }, rpcOptions(request.signal));
      if (request.signal.aborted) return;
      hits = hits.map((hit) => hit.entry?.entryId === entry.entryId ? { ...hit, tags: response.tags } : hit);
      await closeEditor();
    } catch (failure) {
      if (!request.signal.aborted) tagError = fileError(failure);
    } finally {
      if (!request.signal.aborted) saving = false;
    }
  }
</script>

<section class="search-results" aria-labelledby="search-title">
  <div class="drive-heading">
    <div>
      <span class="drive-eyebrow">Shared storage</span>
      <h1 id="search-title">Search results</h1>
    </div>
    <div class="drive-create">
      <button type="button" onclick={onClear}>Clear search</button>
    </div>
  </div>

  <div class="search-body" aria-busy={searching}>
    {#if searching}
      <p role="status">Searching shared files…</p>
    {:else if error}
      <p class="drive-gate-error" role="alert">{error}</p>
    {:else if !hits.length}
      <p role="status">No matching files or folders.</p>
    {:else}
      <p role="status">{hits.length} {hits.length === 1 ? 'match' : 'matches'}{hasMore ? ' shown. Refine your search to find more.' : ''}</p>
      <ol aria-label="Search matches">
        {#each hits as hit (hit.entry?.entryId)}
          {#if hit.entry}
            {@const entry = hit.entry}
            <li>
              <div class="hit">
                <button class="hit-open" type="button" onclick={() => onOpen(entry)}>
                  <Icon name={entry.kind === EntryKind.DIRECTORY ? 'folder' : 'file'} size={18} />
                  <strong>{entry.name}</strong>
                </button>
                <span class="hit-path">{hit.path}</span>
                {#if hit.tags.length}<span class="hit-tags"><strong>Tags</strong> {hit.tags.join(' · ')}</span>{/if}
                <button class="hit-edit" type="button" aria-label={`Edit tags for ${entry.name}`}
                  aria-expanded={editing?.entry?.entryId === entry.entryId} disabled={saving}
                  onclick={(event) => editTags(hit, event.currentTarget)}>Edit tags</button>
              </div>
              {#if editing?.entry?.entryId === entry.entryId}
                <form onsubmit={saveTags}>
                  <label for="search-tags">Tags for {entry.name}</label>
                  <textarea id="search-tags" rows="3" maxlength="2400" bind:this={tagInputElement}
                    bind:value={tagInput} disabled={saving} aria-describedby="search-tags-help"></textarea>
                  <small id="search-tags-help">Enter one tag per line. Clear all lines to remove every tag.</small>
                  {#if tagError}<p class="drive-gate-error" role="alert">{tagError}</p>{/if}
                  <div class="drive-dialog-actions">
                    <button type="button" onclick={closeEditor} disabled={saving}>Cancel</button>
                    <button type="submit" class="drive-primary" disabled={saving}>{saving ? 'Saving…' : 'Save tags'}</button>
                  </div>
                </form>
              {/if}
            </li>
          {/if}
        {/each}
      </ol>
    {/if}
  </div>
</section>

<style>
  .search-results { display: flex; flex: 1; flex-direction: column; min-height: 0; }
  .search-body { overflow: auto; }
  .search-body > p { margin: 0.55rem 0; color: var(--drive-muted); }
  ol { display: grid; margin: 0; padding: 0; list-style: none; }
  li { padding: 0.7rem 0.4rem; border-top: 1px solid var(--drive-border); }
  .hit { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 0.25rem 0.6rem; align-items: start; }
  .hit-open, .hit-edit {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.25rem;
    border: 0;
    border-radius: 0.3rem;
    background: transparent;
    color: var(--drive-text);
    text-align: left;
  }
  .hit-open { justify-self: start; max-width: 100%; }
  .hit-open :global(svg) { flex: none; color: var(--drive-accent); }
  .hit-open strong, .hit-path, .hit-tags { overflow-wrap: anywhere; }
  .hit-open:hover, .hit-edit:hover:enabled { background: var(--drive-hover); }
  .hit-edit { grid-row: 1; grid-column: 2; color: var(--drive-deep); font-size: 0.82rem; }
  .hit-path, .hit-tags { grid-column: 1 / -1; margin-inline-start: 1.85rem; color: var(--drive-muted); font-size: 0.82rem; }
  form { display: grid; gap: 0.35rem; max-width: 25rem; margin: 0.65rem 0 0 1.85rem; }
  form label { font-weight: 650; overflow-wrap: anywhere; }
  form small { color: var(--drive-muted); }
  textarea {
    box-sizing: border-box;
    width: 100%;
    padding: 0.4rem;
    border: 1px solid var(--drive-border);
    border-radius: 0.42rem;
    background: var(--drive-bg);
    color: var(--drive-text);
    font: inherit;
  }
</style>
