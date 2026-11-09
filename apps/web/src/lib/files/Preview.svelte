<script lang="ts">
  import { onMount } from 'svelte';
  import { EntryKind, type Entry } from '../gen/arcdos/files/v1/files_pb';
  import { openMediaStream } from './media';
  import { loadPreview, previewMedia, type ContentLoader, type PreviewContent } from './preview';

  interface Props {
    entry: Entry;
    load: ContentLoader;
    onClose: () => void;
    onDownload: (entry: Entry) => void;
  }

  type PreviewState =
    | Extract<PreviewContent, { kind: 'text' | 'unavailable' }>
    | { kind: 'image' | 'pdf' | 'audio' | 'video'; url: string }
    | { kind: 'loading' };

  let { entry, load, onClose, onDownload }: Props = $props();
  let dialog: HTMLDialogElement;
  let preview: PreviewState = $state({ kind: 'loading' });
  let decodeError = $state(false);
  let mediaFallback: (() => void) | undefined;
  const titleId = $props.id();

  onMount(() => {
    dialog.showModal();
    return () => {
      if (dialog.open) dialog.close();
    };
  });

  $effect(() => {
    const selected = entry;
    const getContent = load;
    const controller = new AbortController();
    const stream = new AbortController();
    let objectUrl: string | undefined;
    preview = { kind: 'loading' };
    decodeError = false;

    async function boundedPreview() {
      try {
        const result = await loadPreview(selected, getContent, controller.signal);
        if (controller.signal.aborted) return;
        if (result.kind === 'pdf' && navigator.pdfViewerEnabled === false) {
          preview = { kind: 'unavailable', message: 'This browser has no built-in PDF viewer.' };
        } else if ('blob' in result) {
          objectUrl = URL.createObjectURL(result.blob);
          preview = { kind: result.kind, url: objectUrl };
        } else {
          preview = result;
        }
      } catch (error) {
        if (controller.signal.aborted) return;
        preview = {
          kind: 'unavailable',
          message: error instanceof Error ? error.message : 'Could not load this preview.'
        };
      }
    }

    async function startPreview() {
      const media = previewMedia(selected.name);
      if (selected.kind === EntryKind.FILE && !selected.trashed && (media?.kind === 'audio' || media?.kind === 'video')) {
        const url = await openMediaStream(selected.entryId, media.type, getContent, stream.signal)
          .catch(() => null);
        if (controller.signal.aborted) return;
        if (url) {
          mediaFallback = () => {
            mediaFallback = undefined;
            stream.abort();
            preview = { kind: 'loading' };
            void boundedPreview();
          };
          preview = { kind: media.kind, url };
          return;
        }
      }
      await boundedPreview();
    }

    void startPreview();

    return () => {
      mediaFallback = undefined;
      controller.abort();
      stream.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  });

  function playbackError() {
    if (mediaFallback) mediaFallback();
    else decodeError = true;
  }

  function cancel(event: Event) {
    event.preventDefault();
    onClose();
  }
</script>

<dialog bind:this={dialog} class="preview" aria-labelledby={titleId} oncancel={cancel}>
  <header>
    <div class="preview-title">
      <p>Preview</p>
      <h2 id={titleId}>{entry.name}</h2>
    </div>
    <button type="button" class="close" aria-label="Close preview" onclick={onClose}>
      <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true">
        <path d="m6 6 12 12M18 6 6 18" />
      </svg>
    </button>
  </header>

  <div class="content" aria-busy={preview.kind === 'loading'}>
    {#if preview.kind === 'loading'}
      <p class="message" role="status">Loading preview…</p>
    {:else if preview.kind === 'unavailable'}
      <p class="message" role="status">{preview.message}</p>
    {:else if decodeError}
      <p class="message" role="status">This browser could not render the file.</p>
    {:else if preview.kind === 'text'}
      {#if preview.truncated}
        <p class="notice">Showing the first 128 KiB. Download to read the full file.</p>
      {/if}
      <pre aria-label="File contents"><code>{preview.text || 'Empty file'}</code></pre>
    {:else if preview.kind === 'image'}
      <img src={preview.url} alt={entry.name} onerror={() => { decodeError = true; }} />
    {:else if preview.kind === 'pdf'}
      <!-- The native viewer isolates PDF content. The URL is a validated, typed PDF Blob. -->
      <iframe src={preview.url} title={`PDF preview of ${entry.name}`} referrerpolicy="no-referrer"></iframe>
    {:else if preview.kind === 'audio'}
      <audio src={preview.url} aria-label={entry.name} controls preload="metadata" onerror={playbackError}></audio>
    {:else if preview.kind === 'video'}
      <!-- svelte-ignore a11y_media_has_caption (Uploaded videos may provide their own embedded caption tracks.) -->
      <video src={preview.url} aria-label={entry.name} controls playsinline preload="metadata" onerror={playbackError}></video>
    {/if}
  </div>

  <footer>
    <button type="button" class="download" onclick={() => onDownload(entry)}>Download</button>
  </footer>
</dialog>

<style>
  .preview {
    box-sizing: border-box;
    width: min(54rem, calc(100vw - 2rem));
    max-height: calc(100dvh - 2rem);
    padding: 0;
    border: 1px solid var(--drive-border, #dce2db);
    border-radius: 1rem;
    color: var(--drive-text, #24342b);
    background: var(--drive-surface, #fff);
    box-shadow: 0 24px 72px #182a2052;
  }

  .preview[open] {
    display: flex;
    flex-direction: column;
  }

  .preview::backdrop {
    background: #14251dc2;
  }

  header, footer {
    display: flex;
    flex: none;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 1rem;
  }

  header {
    border-bottom: 1px solid var(--drive-border, #dce2db);
  }

  footer {
    justify-content: flex-end;
    border-top: 1px solid var(--drive-border, #dce2db);
  }

  .preview-title {
    min-width: 0;
  }

  .preview-title p {
    margin: 0 0 0.125rem;
    font-size: 0.75rem;
    font-weight: 700;
    color: var(--drive-muted, #55665d);
  }

  h2 {
    margin: 0;
    font-size: 1.125rem;
    overflow-wrap: anywhere;
  }

  button {
    flex: none;
    min-height: 2.75rem;
    border: 1px solid transparent;
    border-radius: 0.5rem;
    color: inherit;
    background: transparent;
    font: inherit;
    cursor: pointer;
  }

  button:focus-visible {
    outline: 2px solid var(--drive-accent, #206744);
    outline-offset: 2px;
  }

  .close {
    display: grid;
    width: 2.75rem;
    place-items: center;
  }

  .close:hover {
    background: var(--drive-background, #f3f6f2);
  }

  .download {
    padding-inline: 1rem;
    color: #fff;
    background: var(--drive-accent, #206744);
    font-weight: 650;
  }

  .content {
    display: grid;
    min-height: min(12rem, 40dvh);
    align-items: center;
    overflow: auto;
    background: var(--drive-background, #f3f6f2);
  }

  .message, .notice {
    margin: 0;
    padding: 1rem;
    color: var(--drive-muted, #55665d);
  }

  .message {
    text-align: center;
  }

  .notice {
    padding-bottom: 0;
    font-size: 0.875rem;
  }

  pre {
    box-sizing: border-box;
    margin: 0;
    padding: 1rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    align-self: start;
  }

  img, video {
    display: block;
    max-width: 100%;
    max-height: 67dvh;
    margin: 0 auto;
    object-fit: contain;
  }

  audio {
    width: min(28rem, calc(100% - 2rem));
    margin: 1rem auto;
  }

  iframe {
    display: block;
    width: 100%;
    height: 67dvh;
    border: 0;
    background: #fff;
  }
</style>
