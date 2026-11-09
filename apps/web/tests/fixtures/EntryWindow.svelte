<script lang="ts">
  import { create } from '@bufbuild/protobuf';
  import { SvelteSet } from 'svelte/reactivity';
  import Entries from '../../src/lib/files/Entries.svelte';
  import { EntryKind, EntrySchema, type Entry } from '../../src/lib/gen/arcdos/files/v1/files_pb';

  const entries = Array.from({ length: 100_000 }, (_, index) =>
    create(EntrySchema, {
      entryId: String(index),
      name: `Entry ${String(index).padStart(6, '0')}${index === 1 ? '' : '.png'}`,
      kind: index === 1 ? EntryKind.DIRECTORY : EntryKind.FILE,
      sizeBytes: 14n,
      revision: 'synthetic'
    })
  );
  const thumbnail = new globalThis.Blob([
    Uint8Array.from(
      globalThis.atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9BcAAAAASUVORK5CYII='),
      (character) => character.charCodeAt(0)
    )
  ], { type: 'image/png' });
  const favorites = new SvelteSet<string>();
  const selected = new SvelteSet<string>();
  let view = $state<'list' | 'grid'>('list');
  let opened = $state('');
  let moved = $state('');
  let uploaded = $state('');

  function select(entry: Entry) {
    if (selected.has(entry.entryId)) selected.delete(entry.entryId);
    else selected.add(entry.entryId);
  }
</script>

<button type="button" onclick={() => { view = view === 'list' ? 'grid' : 'list'; }}>
  Toggle view
</button>
<output id="opened">{opened}</output>
<output id="moved">{moved}</output>
<output id="uploaded">{uploaded}</output>

<main>
  <Entries
    {entries}
    {view}
    {selected}
    {favorites}
    onOpen={(entry) => { opened = entry.entryId; }}
    onSelect={select}
    onFavorite={(entry) => { favorites.add(entry.entryId); }}
    onMoveDrop={(target, ids) => { moved = `${target.entryId}:${ids.join(',')}`; }}
    onUploadDrop={(files, target) => { uploaded = `${target?.entryId}:${files[0]?.name}`; }}
    onLoadMore={() => {}}
    hasMore={false}
    loadingMore={false}
    resetKey="fixture"
    loadThumbnail={async () => thumbnail}
  />
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 610px;
    min-height: 0;
  }
</style>
