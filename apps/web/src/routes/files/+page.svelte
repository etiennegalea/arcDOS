<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { EntryKind, type Entry } from '../../lib/gen/arcdos/files/v1/files_pb';
  import { fileError, requireEntry, rpcOptions, type FileClient } from '../../lib/files/client';
  import { Drive, locationFrom, viewEntries, type Section, type Sort } from '../../lib/files/drive.svelte';
  import Entries from '../../lib/files/Entries.svelte';
  import Icon from '../../lib/files/Icon.svelte';
  import Preview from '../../lib/files/Preview.svelte';
  import { loadThumbnail } from '../../lib/files/preview';
  import TransferPanel from '../../lib/files/TransferPanel.svelte';
  import { createTransferQueue, type TransferJob } from '../../lib/files/transfers';
  import './workspace.css';

  const drive = new Drive();
  const sections = [
    { id: 'files', label: 'My files', icon: 'folder' },
    { id: 'recent', label: 'Recent', icon: 'clock' },
    { id: 'favorites', label: 'Favorites', icon: 'star' },
    { id: 'trash', label: 'Trash', icon: 'trash' }
  ] as const;

  type Clipboard = { mode: 'move' | 'copy'; entries: Entry[] };
  type Dialog = { kind: 'create' } | { kind: 'rename'; entry: Entry }
    | { kind: 'trash'; entries: Entry[] };

  let token = $state('');
  let filter = $state('');
  let sort = $state<Sort>('default');
  let selected = $state.raw<ReadonlySet<string>>(new Set());
  let selectionAnchor = '';
  let clipboard = $state.raw<Clipboard | null>(null);
  let preview = $state.raw<Entry | null>(null);
  let dialog = $state.raw<Dialog | null>(null);
  let dialogElement = $state<HTMLDialogElement>();
  let dialogName = $state('');
  let dialogError = $state('');
  let operationBusy = $state(false);
  let notice = $state('');
  let warning = $state('');
  let filterInput = $state<HTMLInputElement>();
  let fileInput = $state<HTMLInputElement>();
  let jobs = $state.raw<readonly TransferJob[]>([]);
  let activeOperation: AbortController | undefined;

  const queue = createTransferQueue((items) => { jobs = items; }, (entry) => {
    drive.remember(entry);
    if (drive.folder?.entryId === entry.parentId || drive.location.section === 'recent') void drive.refresh();
  });

  const section = $derived(drive.location.section);
  const account = $derived(drive.session?.mode === 'oidc' ? drive.session.user : undefined);
  const heading = $derived(section === 'files'
    ? drive.folder?.name || 'My files'
    : sections.find((item) => item.id === section)?.label ?? 'My files');
  const favorites = $derived(new Set(drive.favorites));
  const lookup = $derived(new Map(drive.entries.map((entry) => [entry.entryId, entry])));
  const entries = $derived(viewEntries(
    section === 'favorites' ? drive.entries.filter((entry) => favorites.has(entry.entryId)) : drive.entries,
    filter, sort, section
  ));
  const selection = $derived([...selected].map((id) => lookup.get(id)).filter((entry): entry is Entry => Boolean(entry)));
  const writable = $derived(section === 'files' && drive.folder !== null && !operationBusy);
  const dialogTitle = $derived(dialog?.kind === 'create' ? 'New folder'
    : dialog?.kind === 'rename' ? 'Rename item' : 'Move to trash');
  const pageOptions = $derived(sort === 'default' && section === 'recent' ? 'Recently opened' : 'Name A–Z');

  onMount(() => {
    drive.initialize();
    void drive.loadSession();
    return () => {
      drive.disconnect();
      clearWorkspace();
    };
  });

  $effect(() => {
    if (!drive.client) {
      untrack(clearWorkspace);
      return;
    }
    const url = page.shallow?.url ?? page.url;
    untrack(() => {
      clearSelection();
      filter = '';
      preview = null;
      notice = '';
      warning = '';
      void drive.open(locationFrom(url));
    });
  });

  $effect(() => {
    if (dialog && dialogElement && !dialogElement.open) dialogElement.showModal();
  });

  async function connect(event: SubmitEvent) {
    event.preventDefault();
    const value = token.trim();
    if (!value || drive.connecting) return;
    if (await drive.connect(value)) token = '';
  }

  function clearWorkspace() {
    activeOperation?.abort();
    activeOperation = undefined;
    queue.dispose();
    clearSelection();
    token = '';
    clipboard = null;
    preview = null;
    dialog = null;
    operationBusy = false;
    notice = '';
    warning = '';
  }

  async function navigate(destination: Section, folderId = '') {
    if (drive.location.section === destination && drive.location.folderId === folderId) return;
    const query: '' | `?${string}` = destination !== 'files' ? `?view=${destination}`
      : folderId ? `?folder=${encodeURIComponent(folderId)}` : '';
    try {
      await goto(resolve(`files/${query}`), { shallow: true });
    } catch (error) {
      warning = fileError(error);
    }
  }

  function openEntry(entry: Entry) {
    if (entry.trashed) {
      warning = 'Restore the item to open it.';
    } else if (entry.kind === EntryKind.DIRECTORY) {
      void navigate('files', entry.entryId);
    } else {
      drive.remember(entry);
      preview = entry;
    }
  }

  function clearSelection() {
    selected = new Set();
    selectionAnchor = '';
  }

  function selectEntry(entry: Entry, event: MouseEvent | KeyboardEvent) {
    const next = new SvelteSet(selected);
    const anchor = event.shiftKey ? entries.findIndex((item) => item.entryId === selectionAnchor) : -1;
    if (anchor >= 0) {
      const index = entries.findIndex((item) => item.entryId === entry.entryId);
      const start = Math.min(index, anchor);
      const end = Math.max(index, anchor);
      for (const item of entries.slice(start, end + 1)) next.add(item.entryId);
    } else {
      if (next.has(entry.entryId)) next.delete(entry.entryId);
      else next.add(entry.entryId);
      selectionAnchor = entry.entryId;
    }
    selected = next;
  }

  function pickDialog(next: Dialog) {
    dialogName = next.kind === 'rename' ? next.entry.name : '';
    dialogError = '';
    dialog = next;
  }

  async function runOperation(
    work: (client: FileClient, signal: AbortSignal) => Promise<void>, success: string
  ) {
    const client = drive.client;
    if (!client || operationBusy) return false;
    const operation = new AbortController();
    activeOperation = operation;
    operationBusy = true;
    notice = '';
    warning = '';
    dialogError = '';
    try {
      await work(client, operation.signal);
      if (operation.signal.aborted) return false;
      notice = success;
      clearSelection();
      await drive.refresh();
      return true;
    } catch (error) {
      if (!operation.signal.aborted) {
        if (dialog) dialogError = fileError(error);
        else warning = fileError(error);
        await drive.refresh();
      }
      return false;
    } finally {
      if (!operation.signal.aborted) operationBusy = false;
    }
  }

  async function submitDialog(event: SubmitEvent) {
    event.preventDefault();
    const active = dialog;
    if (!active || operationBusy) return;
    const name = dialogName;
    const parent = drive.folder;
    if (active.kind !== 'trash' && (!name.trim() || name === '.' || name === '..'
      || name.includes('/') || name.includes('\\') || name.includes('\0'))) {
      dialogError = 'Enter a single file or folder name.';
      return;
    }
    let changed = 0;
    const done = await runOperation(async (client, signal) => {
      if (active.kind === 'create' && parent) {
        await client.files.createDirectory({
          parentId: parent.entryId, name, expectedParentRevision: parent.revision
        }, rpcOptions(signal));
      } else if (active.kind === 'rename') {
        const entry = active.entry;
        const destination = parent?.entryId === entry.parentId
          ? parent : await getParent(client, entry.parentId, signal);
        await client.files.moveEntry({
          entryId: entry.entryId, expectedRevision: entry.revision, name,
          destinationParentId: destination.entryId, expectedParentRevision: destination.revision
        }, rpcOptions(signal));
      } else if (active.kind === 'trash') {
        for (const entry of active.entries) {
          try {
            await client.files.trashEntry({
              entryId: entry.entryId, expectedRevision: entry.revision
            }, rpcOptions(signal));
            changed += 1;
          } catch (error) {
            if (changed) throw new Error(`${changed} of ${active.entries.length} moved to trash. ${fileError(error)}`, { cause: error });
            throw error;
          }
        }
      }
    }, active.kind === 'create' ? 'Folder created.' : active.kind === 'rename' ? 'Item renamed.' : 'Moved to trash.');
    if (done || changed > 0) {
      if (!done) warning = dialogError;
      dialog = null;
    }
  }

  function getParent(client: FileClient, entryId: string, signal: AbortSignal) {
    return client.files.getEntry({ entryId }, rpcOptions(signal)).then((response) => requireEntry(response.entry));
  }

  function stash(mode: Clipboard['mode']) {
    const items = selection.filter((entry) => !entry.trashed && (mode !== 'copy' || entry.kind === EntryKind.FILE));
    if (!items.length) return;
    clipboard = { mode, entries: items };
    warning = '';
    notice = `${items.length} ${items.length === 1 ? 'item' : 'items'} ready to ${mode}. Open a destination folder and choose Paste.`;
  }

  async function place(items: Entry[], destination: Entry, mode: Clipboard['mode']) {
    if (destination.kind !== EntryKind.DIRECTORY || destination.trashed) return;
    const completed = new SvelteSet<string>();
    await runOperation(async (client, signal) => {
      for (const item of items) {
        if (item.entryId === destination.entryId) continue;
        try {
          const parent = await getParent(client, destination.entryId, signal);
          const request = {
            entryId: item.entryId, expectedRevision: item.revision, name: item.name,
            destinationParentId: parent.entryId, expectedParentRevision: parent.revision
          };
          if (mode === 'move') await client.files.moveEntry(request, rpcOptions(signal));
          else await client.files.copyEntry(request, rpcOptions(signal));
          completed.add(item.entryId);
        } catch (error) {
          if (completed.size) throw new Error(`${completed.size} of ${items.length} finished. ${fileError(error)}`, { cause: error });
          throw error;
        }
      }
    }, mode === 'move' ? 'Items moved.' : 'Items copied.');
    return completed;
  }

  async function paste() {
    const current = clipboard;
    if (!current || !drive.folder || !writable) return;
    const done = await place(current.entries, drive.folder, current.mode);
    if (done && clipboard === current && current.mode === 'move') {
      const remaining = current.entries.filter((entry) => !done.has(entry.entryId));
      clipboard = remaining.length ? { mode: current.mode, entries: remaining } : null;
    }
  }

  function restore() {
    const items = selection;
    void runOperation(async (client, signal) => {
      let count = 0;
      for (const item of items) {
        try {
          await client.files.restoreEntry({
            entryId: item.entryId, expectedRevision: item.revision
          }, rpcOptions(signal));
          count += 1;
        } catch (error) {
          if (count) throw new Error(`${count} of ${items.length} restored. ${fileError(error)}`, { cause: error });
          throw error;
        }
      }
    }, 'Items restored.');
  }

  function dropMove(target: Entry, ids: string[]) {
    const items = ids.map((id) => lookup.get(id)).filter((entry): entry is Entry => !!entry && entry.entryId !== target.entryId);
    if (writable && items.length) void place(items, target, 'move');
  }

  function upload(files: File[], target = drive.folder) {
    if (writable && drive.client && target && files.length) queue.upload(drive.client, files, target.entryId);
  }

  function uploadSelected(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    if (input.files) upload(Array.from(input.files));
    input.value = '';
  }

  function download(entry: Entry) {
    if (drive.client && entry.kind === EntryKind.FILE && !entry.trashed) {
      drive.remember(entry);
      queue.download(drive.client, entry);
    }
  }

  function thumbnails(entry: Entry, signal: AbortSignal) {
    const client = drive.client;
    return client && !entry.trashed ? loadThumbnail(entry, client.content, signal) : Promise.resolve(null);
  }

  function shortcuts(event: KeyboardEvent) {
    if (!drive.client || dialog || preview || operationBusy || event.defaultPrevented || event.repeat) return;
    const target = event.target;
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      filterInput?.focus();
      return;
    }
    if (target instanceof HTMLElement && (target.isContentEditable
      || target.closest('input, textarea, select, [role="textbox"]'))) return;
    const first = selection[0];
    const key = event.key.toLowerCase();
    if (key === 'escape') clearSelection();
    else if (command && key === 'a') {
      event.preventDefault();
      selected = new Set(entries.map((entry) => entry.entryId));
    } else if (command && key === 'c' && first && !first.trashed) {
      event.preventDefault();
      stash('copy');
    } else if (command && key === 'x' && first && !first.trashed) {
      event.preventDefault();
      stash('move');
    } else if (command && key === 'v' && clipboard && writable) {
      event.preventDefault();
      void paste();
    } else if (key === 'f2' && first && selection.length === 1 && !first.trashed) {
      event.preventDefault();
      pickDialog({ kind: 'rename', entry: first });
    } else if (key === 'delete' && first && !first.trashed) {
      event.preventDefault();
      pickDialog({ kind: 'trash', entries: selection });
    } else if (key === ' ' && first?.kind === EntryKind.FILE && selection.length === 1
      && !(target instanceof HTMLButtonElement)) {
      event.preventDefault();
      openEntry(first);
    }
  }
</script>

<svelte:head>
  <title>Files · ArcDOS</title>
  <meta name="description" content="Browse and manage your shared ArcDOS files." />
</svelte:head>
<svelte:window onkeydown={shortcuts} />

<div class="arc-workspace">
  {#if !drive.client}
    <main class="drive-gate">
      <section class="drive-connect" aria-labelledby="connect-title">
        <div class="drive-brand"><span class="drive-brand-mark"><Icon name="folder" size={22} /></span> ArcDOS</div>
        <div class="drive-connect-title">
          <span class="drive-eyebrow">Private workspace</span>
          <h1 id="connect-title">Connect to your files</h1>
          {#if drive.checkingSession}
            <p role="status">Checking your session…</p>
          {:else if drive.session?.mode === 'oidc'}
            <p>Continue to Authentik to sign in to this workspace.</p>
          {:else if drive.session?.mode === 'development'}
            <p>Enter the development access token for this ArcDOS server.</p>
          {:else if drive.session?.mode === 'disabled'}
            <p>The server has file access disabled.</p>
          {/if}
        </div>
        {#if drive.authError}<p class="drive-gate-error" role="alert">{drive.authError}</p>{/if}
        {#if !drive.checkingSession}
          {#if drive.session?.mode === 'oidc'}
            <form action="/auth/login" method="get" data-sveltekit-reload>
              <button class="drive-primary" type="submit">Sign in with Authentik</button>
            </form>
          {:else if drive.session?.mode === 'development'}
            <form onsubmit={connect}>
              <label for="drive-token">Development token</label>
              <input id="drive-token" type="password" bind:value={token} autocomplete="off" required disabled={drive.connecting} />
              <button class="drive-primary" type="submit" disabled={drive.connecting || !token.trim()}>
                {drive.connecting ? 'Connecting…' : 'Connect'}
              </button>
            </form>
          {:else if !drive.session}
            <button class="drive-primary" type="button" onclick={() => drive.loadSession()}>Check session again</button>
          {/if}
        {/if}
        <a class="drive-back" href={resolve('/')} data-sveltekit-reload>← Server status</a>
      </section>
    </main>
  {:else}
    <aside class="drive-sidebar">
      <div class="drive-brand"><span class="drive-brand-mark"><Icon name="folder" size={21} /></span> ArcDOS</div>
      <span class="drive-eyebrow drive-sidebar-label">Workspace</span>
      <nav aria-label="Workspace">
        {#each sections as item (item.id)}
          <button type="button" class:drive-current={section === item.id} aria-current={section === item.id ? 'page' : undefined}
            onclick={() => navigate(item.id)}>
            <Icon name={item.icon} size={17} />
            <span>{item.label}</span>
          </button>
        {/each}
      </nav>
      <div class="drive-sidebar-foot">
        {#if account}
          <span class="drive-account" title={account.name}><i></i> {account.name}</span>
          <span>{account.roles.includes('drive-admins') ? 'Administrator' : 'Member'}</span>
        {:else}
          <span><i></i> Development session</span>
        {/if}
        <a href={resolve('/')} data-sveltekit-reload>Server status</a>
        <button type="button" disabled={drive.signingOut} onclick={() => drive.signOut()}>
          {drive.signingOut ? 'Signing out…' : account ? 'Sign out' : 'Disconnect'}
        </button>
      </div>
    </aside>

    <main class="drive-main" aria-busy={drive.loading}>
      <header class="drive-top">
        <nav class="drive-crumbs" aria-label="Breadcrumb">
          {#if section === 'files' && drive.breadcrumbs.length}
            {#each drive.breadcrumbs as crumb, index (crumb.entryId)}
              {#if index > 0}<Icon name="chevron-right" size={13} />{/if}
              {#if index === drive.breadcrumbs.length - 1}
                <span aria-current="page">{crumb.name || 'My files'}</span>
              {:else}
                <button type="button" onclick={() => navigate('files', index ? crumb.entryId : '')}>{crumb.name || 'My files'}</button>
              {/if}
            {/each}
          {:else}
            <span aria-current="page">{heading}</span>
          {/if}
        </nav>
        <div class="drive-filter">
          <Icon name="search" size={16} />
          <input type="search" bind:this={filterInput} bind:value={filter} aria-label="Filter files" placeholder="Filter files" />
          <kbd>Ctrl K</kbd>
        </div>
        <button class="drive-mobile-disconnect" type="button" disabled={drive.signingOut} onclick={() => drive.signOut()}>
          {drive.signingOut ? 'Signing out…' : account ? 'Sign out' : 'Disconnect'}
        </button>
      </header>

      <div class="drive-content">
        {#if drive.authError}<p class="drive-gate-error" role="alert">{drive.authError}</p>{/if}
        <div class="drive-heading">
          <div>
            <span class="drive-eyebrow">{section === 'files' ? 'Shared storage' : 'Workspace'}</span>
            <h1>{heading}</h1>
          </div>
          <div class="drive-create">
            {#if section === 'files'}
              <input type="file" multiple hidden bind:this={fileInput} onchange={uploadSelected} />
              <button type="button" disabled={!writable} onclick={() => pickDialog({ kind: 'create' })}>
                <Icon name="folder-plus" size={16} /> New folder
              </button>
              <button class="drive-primary" type="button" disabled={!writable} onclick={() => fileInput?.click()}>
                <Icon name="upload" size={16} /> Upload
              </button>
            {/if}
          </div>
        </div>

        <div class="drive-toolbar">
          <div class="drive-file-actions" aria-label="File actions">
            {#if selection.length}
              <button class="drive-selection" type="button" onclick={clearSelection} title="Clear selection">{selection.length} selected <Icon name="close" size={13} /></button>
              {#if section === 'trash'}
                <button type="button" onclick={restore} disabled={operationBusy}>Restore</button>
              {:else}
                {#if selection.length === 1}
                  <button type="button" disabled={operationBusy} onclick={() => pickDialog({ kind: 'rename', entry: selection[0] })}>Rename</button>
                  {#if selection[0].kind === EntryKind.FILE}
                    <button type="button" onclick={() => download(selection[0])}>Download</button>
                  {/if}
                {/if}
                <button type="button" disabled={operationBusy || !selection.some((entry) => entry.kind === EntryKind.FILE)} onclick={() => stash('copy')}>Copy</button>
                <button type="button" disabled={operationBusy} onclick={() => stash('move')}>Cut</button>
                <button type="button" disabled={operationBusy} onclick={() => pickDialog({ kind: 'trash', entries: selection })}>Trash</button>
              {/if}
            {:else}
              <span class="drive-loaded">{drive.entries.length} {drive.entries.length === 1 ? 'item' : 'items'}{drive.next ? ' loaded' : ''}</span>
            {/if}
            {#if clipboard && section === 'files'}
              <button type="button" disabled={!writable} onclick={paste}>Paste</button>
            {/if}
          </div>
          <div class="drive-display">
            <label class="drive-sort">
              <span class="drive-sr-only">Sort files</span>
              <select bind:value={sort} aria-label="Sort files">
                <option value="default">{pageOptions}</option>
                {#if section === 'recent'}<option value="name">Name A–Z</option>{/if}
                <option value="newest">Newest first</option>
                <option value="largest">Largest first</option>
              </select>
            </label>
            <div class="drive-view-toggle" role="group" aria-label="Layout">
              <button type="button" aria-label="List view" aria-pressed={drive.layout === 'list'} onclick={() => drive.setLayout('list')}><Icon name="list" size={17} /></button>
              <button type="button" aria-label="Grid view" aria-pressed={drive.layout === 'grid'} onclick={() => drive.setLayout('grid')}><Icon name="grid" size={17} /></button>
            </div>
            <button class="drive-icon-button" type="button" aria-label="Refresh" title="Refresh" disabled={drive.loading} onclick={() => drive.refresh()}><Icon name="refresh" size={17} /></button>
          </div>
        </div>

        {#if warning || drive.error || drive.moreError}
          <div class="drive-notice drive-warning" role="alert">
            <span>{warning || drive.error || drive.moreError}</span>
            {#if drive.error || drive.moreError}<button type="button" onclick={() => drive.refresh()}>Reload</button>
            {:else}<button type="button" onclick={() => { warning = ''; }}>Dismiss</button>{/if}
          </div>
        {:else if drive.changesAvailable}
          <div class="drive-notice" role="status"><span>Files changed on the server.</span><button type="button" onclick={() => drive.refresh()}>Refresh files</button></div>
        {:else if notice}
          <div class="drive-notice" role="status"><span>{notice}</span><button type="button" onclick={() => { notice = ''; }}>Dismiss</button></div>
        {/if}

        <div class="drive-entries">
          {#if drive.loading && !drive.entries.length}
            <div class="drive-placeholder" role="status">Loading files…</div>
          {:else}
            <Entries {entries} view={drive.layout} {selected} {favorites}
              resetKey={`${section}/${drive.location.folderId}/${sort}/${filter}`}
              readOnly={!writable} hasMore={!!drive.next && !drive.moreError} loadingMore={drive.loadingMore}
              autoLoad={!filter.trim()} onLoadMore={() => drive.loadMore()}
              onOpen={openEntry} onSelect={selectEntry} onFavorite={(entry) => drive.toggleFavorite(entry)}
              onMoveDrop={dropMove} onUploadDrop={upload} loadThumbnail={thumbnails} />
          {/if}
        </div>

        <footer class="drive-footer">
          <span>{filter.trim() ? `${entries.length} matching loaded items` : drive.next ? 'More items available' : 'Shared filesystem'}</span>
          <span>{drive.loadingMore ? 'Loading next page…' : 'Watching for changes'}</span>
        </footer>
        {#if jobs.length}
          <TransferPanel {jobs} onCancel={queue.cancel} onRetry={queue.retry} onDismiss={queue.dismiss} />
        {/if}
      </div>
    </main>
  {/if}

  {#if preview && drive.client}
    <Preview entry={preview} load={drive.client.content} onClose={() => { preview = null; }} onDownload={download} />
  {/if}

  {#if dialog}
    <dialog class="drive-dialog" bind:this={dialogElement} aria-labelledby="drive-dialog-title"
      oncancel={(event) => { event.preventDefault(); if (!operationBusy) dialog = null; }}>
      <form onsubmit={submitDialog}>
        <h2 id="drive-dialog-title">{dialogTitle}</h2>
        {#if dialog.kind === 'trash'}
          <p>{dialog.entries.length === 1 ? `Move “${dialog.entries[0].name}” to trash?` : `Move ${dialog.entries.length} items to trash?`} You can restore them later.</p>
        {:else}
          <label for="drive-entry-name">{dialog.kind === 'create' ? 'Folder name' : 'Name'}</label>
          <input id="drive-entry-name" type="text" bind:value={dialogName} maxlength="255" required disabled={operationBusy} />
        {/if}
        {#if dialogError}<p class="drive-gate-error" role="alert">{dialogError}</p>{/if}
        <div class="drive-dialog-actions">
          <button type="button" disabled={operationBusy} onclick={() => { dialog = null; }}>Cancel</button>
          <button type="submit" class:drive-danger={dialog.kind === 'trash'} class="drive-primary" disabled={operationBusy}>
            {operationBusy ? 'Working…' : dialog.kind === 'create' ? 'Create folder' : dialog.kind === 'rename' ? 'Save name' : 'Move to trash'}
          </button>
        </div>
      </form>
    </dialog>
  {/if}
</div>
