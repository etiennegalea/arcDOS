<script lang="ts">
  import type { TransferJob, TransferStatus } from './transfers';

  interface Props {
    jobs: readonly TransferJob[];
    onCancel: (id: number) => void;
    onRetry: (id: number) => void;
    onDismiss: (id: number) => void;
  }

  let { jobs, onCancel, onRetry, onDismiss }: Props = $props();
  let collapsed = $state(false);
  let current = $derived(jobs.filter((job) => !['completed', 'failed', 'canceled'].includes(job.status)).length);
  let failed = $derived(jobs.filter((job) => job.status === 'failed').length);

  const labels: Record<TransferStatus, string> = {
    queued: 'Queued',
    choosing: 'Choose where to save',
    hashing: 'Checking file',
    uploading: 'Uploading',
    committing: 'Publishing',
    downloading: 'Downloading',
    canceling: 'Canceling',
    completed: 'Complete',
    failed: 'Interrupted',
    canceled: 'Canceled'
  };

  function percentage(job: TransferJob) {
    if (job.status === 'completed') return 100;
    return job.total ? Math.min(100, Math.floor(job.bytes / job.total * 100)) : 0;
  }

  function isProgress(job: TransferJob) {
    return ['hashing', 'uploading', 'committing', 'downloading'].includes(job.status);
  }
</script>

{#if jobs.length}
  <section class="transfers" aria-label="Transfers">
    <button
      type="button"
      class="panel-header"
      aria-expanded={!collapsed}
      aria-controls="transfer-jobs"
      onclick={() => { collapsed = !collapsed; }}
    >
      <span class="panel-title">Transfers</span>
      <span class="summary">{current} active{failed ? `, ${failed} interrupted` : ''}</span>
      <svg class:rotated={collapsed} width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="m6 9 6 6 6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    <ul id="transfer-jobs" hidden={collapsed}>
      {#each jobs as job (job.id)}
        <li>
          <div class="job-title">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              {#if job.direction === 'upload'}
                <path d="M12 16V4m-5 5 5-5 5 5M4 16v4h16v-4" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
              {:else}
                <path d="M12 4v12m-5-5 5 5 5-5M4 16v4h16v-4" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
              {/if}
            </svg>
            <strong title={job.name}>{job.name}</strong>
            <span class="phase" class:failed={job.status === 'failed'} aria-live="polite">
              {labels[job.status]}
            </span>
          </div>
          {#if isProgress(job)}
            <div class="job-progress">
              <progress aria-label={`${labels[job.status]} ${job.name}`} max="100" value={percentage(job)}></progress>
              <span aria-hidden="true">{percentage(job)}%</span>
            </div>
          {/if}
          {#if job.message}
            <p>{job.message}</p>
          {/if}
          {#if job.canRetry || job.canCancel || job.canDismiss}
            <div class="job-actions">
              {#if job.canRetry}
                <button type="button" aria-label={`Retry ${job.name}`} onclick={() => onRetry(job.id)}>Retry</button>
              {/if}
              {#if job.canCancel}
                <button type="button" aria-label={`Cancel ${job.name}`} onclick={() => onCancel(job.id)}>Cancel</button>
              {/if}
              {#if job.canDismiss}
                <button type="button" aria-label={`Dismiss ${job.name}`} onclick={() => onDismiss(job.id)}>Dismiss</button>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .transfers {
    position: fixed;
    z-index: 5;
    inset-block-end: 1rem;
    inset-inline-end: 1rem;
    width: min(24rem, calc(100vw - 2rem));
    overflow: hidden;
    border: 1px solid var(--drive-border, #dbe1db);
    border-radius: .75rem;
    color: var(--drive-text, #24332a);
    background: var(--drive-surface, #fff);
    box-shadow: 0 .6rem 2rem #1a38252a;
  }

  button {
    font: inherit;
    cursor: pointer;
  }

  button:focus-visible {
    outline: 2px solid var(--drive-accent, #246146);
    outline-offset: -2px;
  }

  .panel-header {
    display: flex;
    align-items: center;
    gap: .55rem;
    width: 100%;
    min-height: 2.75rem;
    padding: .55rem .8rem;
    border: 0;
    color: inherit;
    text-align: start;
    background: var(--drive-soft, #edf4ee);
  }

  .panel-title {
    font-weight: 650;
  }

  .summary {
    flex: 1;
    color: var(--drive-muted, #516458);
    font-size: .75rem;
  }

  svg {
    flex: none;
  }

  .rotated {
    transform: rotate(180deg);
  }

  ul {
    max-height: min(20rem, 42vh);
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }

  li {
    padding: .65rem .8rem;
  }

  li + li {
    border-top: 1px solid var(--drive-border, #dbe1db);
  }

  .job-title,
  .job-progress,
  .job-actions {
    display: flex;
    align-items: center;
    gap: .45rem;
  }

  strong {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    font-size: .85rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .phase,
  .job-progress,
  p {
    color: var(--drive-muted, #516458);
    font-size: .75rem;
  }

  .phase.failed {
    color: #8d3828;
  }

  .job-progress,
  .job-actions,
  p {
    margin: .35rem 0 0;
  }

  progress {
    flex: 1;
    min-width: 0;
    height: .45rem;
    accent-color: var(--drive-accent, #246146);
  }

  .job-actions {
    justify-content: flex-end;
  }

  .job-actions button {
    min-height: 1.8rem;
    padding: .1rem .45rem;
    border: 1px solid var(--drive-border, #dbe1db);
    border-radius: .35rem;
    color: var(--drive-text, #24332a);
    background: var(--drive-surface, #fff);
    font-size: .75rem;
  }
</style>
