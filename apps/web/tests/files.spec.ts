import { expect, test, type Page } from '@playwright/test';

const token = process.env.ARCDOS_E2E_DEV_TOKEN ?? '';
const newName = () => `browser-${crypto.randomUUID().slice(0, 8)}`;

async function connect(page: Page, fromStatus = false) {
  if (fromStatus) {
    await page.goto('/');
    await page.getByRole('link', { name: 'Open file manager' }).click();
  } else {
    await page.goto('/files/');
  }
  await page.getByLabel('Development token').fill(token);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'My files', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'New folder' })).toBeEnabled();
}

async function createFolder(page: Page, name: string) {
  await page.getByRole('button', { name: 'New folder' }).click();
  const dialog = page.getByRole('dialog', { name: 'New folder' });
  await dialog.getByLabel('Folder name').fill(name);
  await dialog.getByRole('button', { name: 'Create folder' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('button', { name, exact: true })).toBeVisible();
}

async function openFolder(page: Page, name: string) {
  await page.getByRole('button', { name, exact: true }).click();
  await expect(page.getByRole('heading', { name, exact: true })).toBeVisible();
}

test.describe('file manager against synthetic storage', () => {
  test.skip(!token, 'Set ARCDOS_E2E_DEV_TOKEN for an isolated file API test instance.');

  test('uploads, previews, downloads, copies, renames, trashes and restores files', async ({ page }) => {
    test.setTimeout(60_000);
    await connect(page, true);
    const folder = newName();
    await createFolder(page, folder);
    await openFolder(page, folder);
    await expect(page).toHaveURL(/folder=/);
    await page.goBack();
    await expect(page.getByRole('heading', { name: 'My files' })).toBeVisible();
    await page.goForward();
    await expect(page.getByRole('heading', { name: folder })).toBeVisible();

    await page.locator('input[type="file"]').setInputFiles({
      name: 'hello.txt', mimeType: 'text/plain', buffer: Buffer.from('ArcDOS browser fixture')
    });
    await expect(page.getByRole('button', { name: 'hello.txt', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'hello.txt', exact: true }).click();
    const preview = page.getByRole('dialog', { name: 'hello.txt' });
    await expect(preview.getByLabel('File contents')).toHaveText('ArcDOS browser fixture');
    const downloadEvent = page.waitForEvent('download');
    await preview.getByRole('button', { name: 'Download', exact: true }).click();
    const download = await downloadEvent;
    expect(download.suggestedFilename()).toBe('hello.txt');
    await preview.getByRole('button', { name: 'Close preview' }).click();

    const workspace = page.getByRole('navigation', { name: 'Workspace' });
    await page.getByRole('button', { name: 'Favorite hello.txt', exact: true }).click();
    await workspace.getByRole('button', { name: 'Favorites' }).click();
    await expect(page.getByRole('button', { name: 'hello.txt', exact: true })).toBeVisible();
    await workspace.getByRole('button', { name: 'Recent' }).click();
    await expect(page.getByRole('button', { name: 'hello.txt', exact: true })).toBeVisible();
    await workspace.getByRole('button', { name: 'My files' }).click();
    await openFolder(page, folder);

    await createFolder(page, 'Destination');
    await page.getByRole('checkbox', { name: 'Select hello.txt' }).click();
    await page.getByRole('button', { name: 'Copy', exact: true }).click();
    await openFolder(page, 'Destination');
    await page.getByRole('button', { name: 'Paste', exact: true }).click();
    await page.getByRole('checkbox', { name: 'Select hello.txt' }).click();
    await page.getByRole('button', { name: 'Rename', exact: true }).click();
    const rename = page.getByRole('dialog', { name: 'Rename item' });
    await rename.getByLabel('Name', { exact: true }).fill('renamed.txt');
    await rename.getByRole('button', { name: 'Save name' }).click();
    await expect(page.getByRole('button', { name: 'renamed.txt', exact: true })).toBeVisible();
    await page.getByRole('checkbox', { name: 'Select renamed.txt' }).click();
    await page.getByRole('button', { name: 'Trash', exact: true }).last().click();
    await page.getByRole('dialog', { name: 'Move to trash' }).getByRole('button', { name: 'Move to trash' }).click();
    await expect(page.getByRole('button', { name: 'renamed.txt', exact: true })).toBeHidden();
    await workspace.getByRole('button', { name: 'Trash' }).click();
    await page.getByRole('checkbox', { name: 'Select renamed.txt' }).click();
    await page.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(page.getByRole('button', { name: 'renamed.txt', exact: true })).toBeHidden();

    await workspace.getByRole('button', { name: 'My files' }).click();
    await openFolder(page, folder);
    await openFolder(page, 'Destination');
    await expect(page.getByRole('button', { name: 'renamed.txt', exact: true })).toBeVisible();
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole('button', { name: 'Grid view' }).click();
    await expect(page.getByRole('button', { name: 'Grid view' })).toHaveAttribute('aria-pressed', 'true');
    await page.getByRole('searchbox', { name: 'Filter files' }).fill('nothing matches this');
    await expect(page.getByRole('button', { name: 'renamed.txt', exact: true })).toBeHidden();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(await page.evaluate((secret) => JSON.stringify(localStorage).includes(secret), token)).toBe(false);
  });

  test('notifies when a second client changes the folder', async ({ page, request }) => {
    const watched = page.waitForResponse('**/arcdos.files.v1.FileService/ListChanges');
    await connect(page);
    await watched;
    const headers = { Authorization: `Bearer ${token}`, 'Connect-Protocol-Version': '1' };
    const root = await request.post('/arcdos.files.v1.FileService/GetEntry', { headers, data: {} });
    expect(root.ok()).toBe(true);
    const folder = newName();
    const revision = (await root.json()).entry.revision as string;
    const created = await request.post('/arcdos.files.v1.FileService/CreateDirectory', {
      headers, data: { name: folder, expectedParentRevision: revision }
    });
    expect(created.ok()).toBe(true);
    await page.getByRole('button', { name: 'Refresh files' }).click({ timeout: 15_000 });
    await expect(page.getByRole('button', { name: folder, exact: true })).toBeVisible();
  });
});
