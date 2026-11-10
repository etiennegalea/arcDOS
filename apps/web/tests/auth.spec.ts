import { create, toBinary, type DescMessage, type MessageInitShape } from '@bufbuild/protobuf';
import { expect, test, type Page } from '@playwright/test';
import {
  BeginUploadResponseSchema, CommitUploadResponseSchema, EntryKind, EntrySchema,
  GetEntryResponseSchema, ListChangesResponseSchema, ListEntriesResponseSchema, UploadSchema
} from '../src/lib/gen/arcdos/files/v1/files_pb';

const csrfToken = 'synthetic-csrf-value';
const sessionCookie = 'arcdos_fixture_session=synthetic-session';
const fileName = 'uploaded.txt';
const login = 'Sign in with Authentik';

const wire = <Descriptor extends DescMessage>(schema: Descriptor, data?: MessageInitShape<Descriptor>) => ({
  contentType: 'application/proto',
  body: Buffer.from(toBinary(schema, create(schema, data)))
});

async function mockSignedIn(page: Page, baseURL: string) {
  let authenticated = true;
  let uploaded = false;
  let bulkRevoked = false;
  const calls: { operation: string; headers: Record<string, string> }[] = [];
  const root = create(EntrySchema, { entryId: 'root', revision: 'root-revision', kind: EntryKind.DIRECTORY });
  const file = create(EntrySchema, {
    entryId: 'fixture-file', parentId: root.entryId, name: fileName,
    revision: 'file-revision', kind: EntryKind.FILE, sizeBytes: 3n
  });

  await page.context().addCookies([{
    name: 'arcdos_fixture_session', value: 'synthetic-session', url: baseURL, httpOnly: true, sameSite: 'Lax'
  }]);
  await page.route('**/auth/session', (route) => route.fulfill({
    json: authenticated
      ? { mode: 'oidc', user: { name: 'Test Administrator', roles: ['drive-admins'] }, csrf_token: csrfToken }
      : { mode: 'oidc', user: null, csrf_token: null }
  }));
  await page.route('**/auth/logout', async (route) => {
    calls.push({ operation: route.request().method() + ' logout', headers: await route.request().allHeaders() });
    authenticated = false;
    await route.fulfill({ status: 204 });
  });
  await page.route(/\/arcdos\.files\.v1\.(FileService|TransferService)\/[A-Za-z]+$/, async (route) => {
    const procedure = new URL(route.request().url()).pathname.split('/').at(-1);
    calls.push({ operation: route.request().method() + ' ' + procedure, headers: await route.request().allHeaders() });
    if (!authenticated) {
      await route.fulfill({ status: 401, json: { code: 'unauthenticated', message: 'Expired test session' } });
      return;
    }
    switch (procedure) {
      case 'GetEntry':
        await route.fulfill(wire(GetEntryResponseSchema, { entry: root }));
        break;
      case 'ListEntries':
        await route.fulfill(wire(ListEntriesResponseSchema, { entries: uploaded ? [file] : [] }));
        break;
      case 'ListChanges':
        await route.fulfill(wire(ListChangesResponseSchema));
        break;
      case 'BeginUpload':
        await route.fulfill(wire(BeginUploadResponseSchema, {
          upload: create(UploadSchema, { uploadId: 'fixture-upload', sizeBytes: 3n })
        }));
        break;
      case 'CommitUpload':
        uploaded = true;
        await route.fulfill(wire(CommitUploadResponseSchema, { entry: file }));
        break;
      default:
        await route.fulfill({ status: 501, json: { code: 'unimplemented' } });
    }
  });
  await page.route('**/api/files/v1/uploads/fixture-upload', async (route) => {
    calls.push({ operation: route.request().method() + ' upload', headers: await route.request().allHeaders() });
    await route.fulfill({ status: 204, headers: { 'Upload-Offset': '3' } });
  });
  await page.route('**/api/files/v1/fixture-file/content', async (route) => {
    calls.push({ operation: route.request().method() + ' content', headers: await route.request().allHeaders() });
    await route.fulfill(bulkRevoked
      ? { status: 401 }
      : { body: 'abc', contentType: 'application/octet-stream' });
  });
  return {
    calls, revokeSession: () => { authenticated = false; },
    revokeBulk: () => { bulkRevoked = true; }
  };
}

test('starts Authentik sign-in with a same-origin navigation', async ({ page }) => {
  await page.route('**/auth/session', (route) => route.fulfill({ json: { mode: 'oidc' } }));
  await page.route(/\/auth\/login\??$/, (route) => route.fulfill({
    body: '<!doctype html><title>Synthetic provider redirect</title>', contentType: 'text/html'
  }));
  await page.goto('/files/');
  await expect(page.getByLabel('Development token')).toHaveCount(0);
  const request = page.waitForRequest(/\/auth\/login\??$/);
  await page.getByRole('button', { name: login }).click();
  expect((await request).method()).toBe('GET');
  await expect(page).toHaveTitle('Synthetic provider redirect');
});

test('uses cookie and CSRF headers for mutations and signs out locally', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('The Playwright base URL is required.');
  const { calls } = await mockSignedIn(page, baseURL);
  await page.goto('/files/');
  await expect(page.getByText('Test Administrator')).toBeVisible();
  await expect(page.getByText('Administrator', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Upload', exact: true })).toBeEnabled();
  await page.locator('input[type="file"]').setInputFiles({
    name: fileName, mimeType: 'text/plain', buffer: Buffer.from('abc')
  });
  await page.getByRole('button', { name: fileName, exact: true }).click();
  const preview = page.getByRole('dialog', { name: fileName });
  await expect(preview.getByLabel('File contents')).toHaveText('abc');
  await preview.getByRole('button', { name: 'Close preview' }).click();
  await page.getByRole('button', { name: 'Sign out', exact: true }).click();
  await expect(page.getByRole('button', { name: login })).toBeVisible();
  await expect(page.getByText('Test Administrator')).toHaveCount(0);
  expect(await page.evaluate((csrf) => {
    return [localStorage, sessionStorage].every((storage) => !JSON.stringify(storage).includes(csrf));
  }, csrfToken)).toBe(true);
  for (const operation of ['POST ListEntries', 'POST BeginUpload', 'PATCH upload', 'POST CommitUpload', 'POST logout']) {
    const request = calls.find((call) => call.operation === operation);
    expect(request?.headers['x-csrf-token'], operation).toBe(csrfToken);
    expect(request?.headers.cookie, operation).toContain(sessionCookie);
    expect(request?.headers.authorization, operation).toBeUndefined();
  }
  const content = calls.find((call) => call.operation === 'GET content');
  expect(content?.headers.cookie).toContain(sessionCookie);
  expect(content?.headers['x-csrf-token']).toBeUndefined();
  expect(content?.headers.authorization).toBeUndefined();
});

test('clears the workspace after a rejected Connect session', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('The Playwright base URL is required.');
  const { revokeSession } = await mockSignedIn(page, baseURL);
  await page.goto('/files/');
  await expect(page.getByRole('button', { name: 'New folder' })).toBeEnabled();
  revokeSession();
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(page.getByRole('alert')).toHaveText('Your session ended. Sign in to continue.');
  await expect(page.getByRole('button', { name: login })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Workspace' })).toHaveCount(0);
});

test('clears the workspace after a rejected bulk session', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('The Playwright base URL is required.');
  const { revokeBulk } = await mockSignedIn(page, baseURL);
  await page.goto('/files/');
  await expect(page.getByRole('button', { name: 'Upload', exact: true })).toBeEnabled();
  await page.locator('input[type="file"]').setInputFiles({
    name: fileName, mimeType: 'text/plain', buffer: Buffer.from('abc')
  });
  revokeBulk();
  await page.getByRole('button', { name: fileName, exact: true }).click();
  await expect(page.getByRole('button', { name: login })).toBeVisible();
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('keeps development access behind an explicit server mode', async ({ page }) => {
  await page.route('**/auth/session', (route) => route.fulfill({ status: 503 }));
  await page.goto('/files/');
  await expect(page.getByRole('alert')).toHaveText('Could not check your session. Try again.');
  await expect(page.getByLabel('Development token')).toHaveCount(0);
  await page.route('**/auth/session', (route) => route.fulfill({ json: { mode: 'development' } }));
  await page.getByRole('button', { name: 'Check session again' }).click();
  await expect(page.getByLabel('Development token')).toBeVisible();
  await expect(page.getByRole('button', { name: login })).toHaveCount(0);
});
