import { writeFile } from 'node:fs/promises';
import { availableParallelism, cpus } from 'node:os';
import { performance } from 'node:perf_hooks';
import { fromBinary } from '@bufbuild/protobuf';
import { expect, test, type Locator, type Page } from '@playwright/test';
import { ListChangesResponseSchema, ListEntriesResponseSchema } from '../../src/lib/gen/arcdos/files/v1/files_pb';

const LIST_RPC = '/arcdos.files.v1.FileService/ListEntries';
const CHANGES_RPC = '/arcdos.files.v1.FileService/ListChanges';
const SPEED_PX_PER_SECOND = 1800;

function integerSetting(name: string, fallback: number, min: number, max: number): number {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new Error(`${name} must be an integer from ${min} to ${max}.`);
  }
  return value;
}

function percentile(values: readonly number[], fraction: number) {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * fraction) - 1];
}

async function nextListing(page: Page, action: () => Promise<unknown>) {
  const [response] = await Promise.all([
    page.waitForResponse((candidate) =>
      candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === LIST_RPC
    ),
    action()
  ]);
  expect(response.ok(), `ListEntries returned HTTP ${response.status()}`).toBe(true);
  const listing = fromBinary(ListEntriesResponseSchema, await response.body());
  expect(listing.entries.length).toBeGreaterThan(0);
  return {
    entries: listing.entries.length,
    hasMore: Boolean(listing.nextPageToken),
    requestMs: response.request().timing().responseEnd
  };
}

async function waitForListRows(list: Locator, entries: number) {
  await expect.poll(() => list.evaluate((node) => {
    const element = node as HTMLElement;
    const height = Number.parseFloat(element.parentElement?.style.height ?? '');
    const rowHeight = Number.parseFloat(element.style.getPropertyValue('--entry-height'));
    return Math.round(height / rowHeight);
  }), { timeout: 20_000 }).toBe(entries);
}

async function frameSample(viewport: Locator, durationMs: number) {
  const { intervals, ...sample } = await viewport.evaluate((node, { durationMs, speed }) => {
    const scroller = node as HTMLElement;
    const list = scroller.querySelector('ul');
    if (!list) throw new Error('The file window is missing.');
    const rowHeight = Number.parseFloat(list.style.getPropertyValue('--entry-height'));
    // Leave a three-row margin before the infinite-scroll loading boundary.
    const distance = Math.floor(Math.min(durationMs * speed / 1000,
      scroller.scrollHeight - scroller.clientHeight - 3 * rowHeight));
    if (distance <= rowHeight) throw new Error('The benchmark folder needs more rows to scroll.');
    return new Promise<{
      intervals: number[];
      scrollPx: number;
      peakMountedEntries: number;
      lastWindowStart: number;
    }>((resolve) => {
      const intervals: number[] = [];
      let peakMountedEntries = 0;
      let lastWindowStart = 0;
      globalThis.requestAnimationFrame((start) => {
        let previous = start;
        function frame(now: number) {
          intervals.push(now - previous);
          previous = now;
          peakMountedEntries = Math.max(peakMountedEntries, list!.childElementCount);
          lastWindowStart = Number(list!.firstElementChild?.getAttribute('data-entry-index') ?? 0);
          const elapsed = now - start;
          if (elapsed >= durationMs) {
            resolve({ intervals, scrollPx: scroller.scrollTop, peakMountedEntries, lastWindowStart });
            return;
          }
          scroller.scrollTop = Math.round(distance * elapsed / durationMs);
          globalThis.requestAnimationFrame(frame);
        }
        globalThis.requestAnimationFrame(frame);
      });
    });
  }, { durationMs, speed: SPEED_PX_PER_SECOND });
  const elapsedMs = intervals.reduce((sum, interval) => sum + interval, 0);
  return {
    ...sample,
    elapsedMs,
    frames: intervals.length,
    fps: intervals.length * 1000 / elapsedMs,
    frameMsP50: percentile(intervals, 0.5),
    frameMsP95: percentile(intervals, 0.95),
    frameMsP99: percentile(intervals, 0.99),
    framesOver25Ms: intervals.filter((interval) => interval > 25).length
  };
}

test('measure scrolling and virtualization on a real flat directory', async ({ page, browser }, info) => {
  const token = process.env.ARCDOS_E2E_DEV_TOKEN;
  if (!token) throw new Error('Set ARCDOS_E2E_DEV_TOKEN for a disposable benchmark server.');
  const directory = process.env.ARCDOS_BENCH_DIRECTORY ?? 'flat';
  const pageLimit = integerSetting('ARCDOS_BENCH_PAGES', 10, 1, 1000);
  const durationMs = integerSetting('ARCDOS_BENCH_FRAME_MS', 3000, 500, 15000);
  const expectedEntries = integerSetting('ARCDOS_BENCH_ENTRIES', 0, 0, 1_000_000);

  await page.goto('/files/');
  await page.getByLabel('Development token').fill(token);
  const journalAt = performance.now();
  const journalTail = page.waitForResponse(async (response) => {
    if (!response.ok() || response.request().method() !== 'POST' ||
      new URL(response.url()).pathname !== CHANGES_RPC) return false;
    const changes = fromBinary(ListChangesResponseSchema, await response.body());
    return changes.changes.length < 500;
  }, { timeout: 120_000 });
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  const folder = page.getByRole('button', { name: directory, exact: true });
  await expect(folder).toBeVisible({ timeout: 20_000 });
  await journalTail;
  const initialJournalReadyMs = performance.now() - journalAt;
  const viewport = page.getByRole('region', { name: 'File entries' });
  const list = viewport.getByRole('list', { name: 'Files and folders' });
  const firstFile = list.getByRole('button', { name: 'file-000000.txt', exact: true });

  const firstAt = performance.now();
  let listing = await nextListing(page, () => folder.click());
  await expect(firstFile).toBeVisible({ timeout: 20_000 });
  await waitForListRows(list, listing.entries);
  const firstPage = { entries: listing.entries, readyMs: performance.now() - firstAt, requestMs: listing.requestMs };

  let loadedEntries = listing.entries;
  const appendMs: number[] = [];
  const appendRequestMs: number[] = [];
  for (let loadedPages = 1; listing.hasMore && loadedPages < pageLimit; loadedPages++) {
    const started = performance.now();
    listing = await nextListing(page, () => viewport.evaluate((node) => { node.scrollTop = node.scrollHeight; }));
    loadedEntries += listing.entries;
    await waitForListRows(list, loadedEntries);
    appendMs.push(performance.now() - started);
    appendRequestMs.push(listing.requestMs);
  }
  if (expectedEntries) {
    if (listing.hasMore) expect(loadedEntries).toBeLessThan(expectedEntries);
    else expect(loadedEntries).toBe(expectedEntries);
  }

  const layouts = [];
  for (const layout of ['list', 'grid'] as const) {
    const toggle = page.getByRole('button', { name: `${layout === 'list' ? 'List' : 'Grid'} view` });
    if (layout === 'grid') await toggle.click();
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await viewport.evaluate((node) => { node.scrollTop = 0; });
    await expect(firstFile).toBeVisible({ timeout: 20_000 });
    const sample = await frameSample(viewport, durationMs);
    expect(sample.scrollPx).toBeGreaterThan(0);
    expect(sample.lastWindowStart).toBeGreaterThan(0);
    expect(sample.peakMountedEntries).toBeLessThan(Math.min(loadedEntries, 160));
    layouts.push({ layout, ...sample });
  }

  const report = {
    version: 1,
    environment: {
      node: process.version,
      platform: process.platform,
      architecture: process.arch,
      cpu: cpus()[0]?.model,
      availableCpus: availableParallelism(),
      browser: browser.version(),
      channel: process.env.ARCDOS_E2E_BROWSER_CHANNEL ?? 'bundled',
      headless: true,
      viewport: page.viewportSize()
    },
    workload: {
      directory,
      expectedEntries: expectedEntries || null,
      loadedEntries,
      loadedPages: appendMs.length + 1,
      hasMore: listing.hasMore,
      initialJournalReadyMs,
      requestedFrameSampleMs: durationMs,
      targetSpeedPxPerSecond: SPEED_PX_PER_SECOND
    },
    firstPage,
    paging: { readyMsP95: percentile(appendMs, 0.95), requestMsP95: percentile(appendRequestMs, 0.95) },
    layouts
  };
  const output = info.outputPath('scroll.json');
  await writeFile(output, `${JSON.stringify(report, null, 2)}\n`);
  process.stdout.write(`ARCDOS_SCROLL_BENCH ${JSON.stringify(report)}\nSaved to ${output}\n`);
});
