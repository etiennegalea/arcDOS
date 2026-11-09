import { fileURLToPath } from 'node:url';
import { expect, test } from '@playwright/test';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createServer, type ViteDevServer } from 'vite';

let server: ViteDevServer | undefined;
let fixtureUrl: string;

test.beforeAll(async () => {
  server = await createServer({
    root: fileURLToPath(new URL('../', import.meta.url)),
    configFile: false,
    publicDir: false,
    cacheDir: 'node_modules/.vite-entry-fixture',
    logLevel: 'warn',
    plugins: [svelte()],
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { host: '127.0.0.1', port: 0, hmr: false, watch: null }
  });
  await server.listen();
  const address = server.httpServer?.address();
  if (!address || typeof address === 'string') throw new Error('Entry fixture could not listen');
  fixtureUrl = `http://127.0.0.1:${address.port}/tests/fixtures/entries.html`;
});

test.afterAll(async () => {
  await server?.close();
});

test('keeps a 100,000-entry directory virtualized while navigating, resizing, and dropping', async ({ page }) => {
  await page.setViewportSize({ width: 1140, height: 770 });
  await page.goto(fixtureUrl);

  const first = page.getByRole('button', { name: 'Entry 000000.png', exact: true });
  const last = page.getByRole('button', { name: 'Entry 099999.png', exact: true });
  const rows = page.getByRole('list', { name: 'Files and folders' }).getByRole('listitem');
  await expect(first).toBeVisible({ timeout: 15_000 });
  expect(await rows.count()).toBeLessThan(50);

  await first.press('End');
  await expect(last).toBeFocused();
  expect(await rows.count()).toBeLessThan(50);
  await last.press('Space');
  await expect(page.getByRole('checkbox', { name: 'Select Entry 099999.png' })).toBeChecked();
  await last.press('Enter');
  await expect(page.locator('#opened')).toHaveText('99999');

  await page.getByRole('button', { name: 'Toggle view' }).click();
  await expect(first).toBeVisible();
  expect(await rows.count()).toBeLessThan(100);
  await expect(first.locator('img')).toHaveJSProperty('naturalWidth', 1);

  const folder = page.getByRole('button', { name: 'Entry 000001', exact: true });
  await first.dragTo(folder);
  await expect(page.locator('#moved')).toHaveText('1:0');

  const transfer = await page.evaluateHandle(() => {
    const data = new DataTransfer();
    data.items.add(new File(['example'], 'synthetic.txt'));
    return data;
  });
  await folder.dispatchEvent('drop', { dataTransfer: transfer });
  await transfer.dispose();
  await expect(page.locator('#uploaded')).toHaveText('1:synthetic.txt');

  await page.setViewportSize({ width: 390, height: 770 });
  await expect.poll(() => rows.first().evaluate((row) =>
    getComputedStyle(row.parentElement!).gridTemplateColumns.trim().split(/\s+/).length
  )).toBe(2);
  expect(await rows.count()).toBeLessThan(40);
});
