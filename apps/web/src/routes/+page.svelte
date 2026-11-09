<script lang="ts">
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import { checkHealth } from '../lib/health';

  type HealthStatus = 'checking' | 'ready' | 'unavailable';

  const messages: Record<HealthStatus, { label: string; detail: string }> = {
    checking: { label: 'Checking', detail: 'Contacting the ArcDOS server.' },
    ready: { label: 'Ready', detail: 'The ArcDOS server responded to the health check.' },
    unavailable: { label: 'Unavailable', detail: 'The server did not report ready. Try checking again.' }
  };

  let health = $state<HealthStatus>('checking');
  let controller: AbortController | undefined;

  async function refreshHealth() {
    controller?.abort();
    const request = new AbortController();
    controller = request;
    health = 'checking';

    try {
      const ready = await checkHealth(window.location.origin, request.signal);
      if (!request.signal.aborted) health = ready ? 'ready' : 'unavailable';
    } catch {
      if (!request.signal.aborted) health = 'unavailable';
    }
  }

  onMount(() => {
    void refreshHealth();
    return () => controller?.abort();
  });
</script>

<svelte:head>
  <title>ArcDOS · Server status</title>
  <meta name="description" content="Check the status of your ArcDOS server." />
</svelte:head>

<main>
  <header class="brand">
    <svg width="40" height="40" viewBox="0 0 40 40" fill="none" aria-hidden="true">
      <rect width="40" height="40" rx="10" fill="#1d4537" />
      <path d="M10 17a2 2 0 0 1 2-2h5l2 2h9a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H12a2 2 0 0 1-2-2z" stroke="#edf4ec" stroke-width="1.7" stroke-linejoin="round" />
    </svg>
    <span>ArcDOS</span>
  </header>

  <section aria-labelledby="health-title">
    <h1 id="health-title">Server status</h1>
    <div class="status" role="status">
      <div class="indicator" class:ready={health === 'ready'} class:unavailable={health === 'unavailable'}>
        <span class="dot" aria-hidden="true"></span>
        {messages[health].label}
      </div>
      <p>{messages[health].detail}</p>
    </div>
    <div class="actions">
      <button type="button" onclick={refreshHealth} disabled={health === 'checking'}>Check again</button>
      <a href={resolve('files/')} data-sveltekit-reload>Open file manager <span aria-hidden="true">→</span></a>
    </div>
  </section>
</main>

<style>
  :global(html) {
    color: #203128;
    background: #f4f7f1;
    font-family: system-ui, sans-serif;
    line-height: 1.5;
  }

  :global(body) {
    margin: 0;
  }

  main {
    max-width: 38rem;
    margin: clamp(3rem, 12vh, 8rem) auto;
    padding-inline: 1.25rem;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 2rem;
    font-size: 1.25rem;
    font-weight: 700;
  }

  section {
    padding: clamp(1.25rem, 5vw, 2.5rem);
    border: 1px solid #d6e0d4;
    border-radius: 1rem;
    background: #fff;
  }

  h1 {
    margin: 0 0 1.25rem;
    font-size: clamp(1.75rem, 6vw, 2.5rem);
    letter-spacing: -0.035em;
    line-height: 1.15;
  }

  .indicator {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.25rem 0.65rem;
    border-radius: 1rem;
    background: #edf0ec;
    color: #41534a;
    font-size: 0.875rem;
    font-weight: 650;
  }

  .ready {
    background: #e6f2e9;
    color: #195634;
  }

  .unavailable {
    background: #f8eae6;
    color: #843c2b;
  }

  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: currentColor;
  }

  p {
    margin: 0.75rem 0 1.5rem;
    color: #41534a;
  }

  button {
    min-height: 2.75rem;
    padding: 0.5rem 1rem;
    border: 1px solid #1d4537;
    border-radius: 0.45rem;
    color: #fff;
    background: #1d4537;
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.65;
    cursor: default;
  }

  button:focus-visible {
    outline: 2px solid #1d4537;
    outline-offset: 3px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 1rem;
  }

  .actions a {
    color: #1d4537;
    font-weight: 650;
    text-underline-offset: 0.2em;
  }
</style>
