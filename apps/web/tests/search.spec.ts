import { create, fromBinary, toBinary, type DescMessage, type MessageInitShape } from '@bufbuild/protobuf';
import { expect, test, type Page } from '@playwright/test';
import {
  EntryKind, EntrySchema, GetEntryRequestSchema, GetEntryResponseSchema, ListChangesResponseSchema,
  ListEntriesResponseSchema
} from '../src/lib/gen/arcdos/files/v1/files_pb';
import {
  SearchFilesRequestSchema, SearchFilesResponseSchema, SetTagsRequestSchema, SetTagsResponseSchema
} from '../src/lib/gen/arcdos/search/v1/search_pb';

const csrf = 'synthetic-search-csrf';
const searchRpc = '**/arcdos.search.v1.SearchService/SearchFiles';
const tagsRpc = '**/arcdos.search.v1.SearchService/SetTags';
const root = create(EntrySchema, { entryId: 'root', revision: 'root-r1', kind: EntryKind.DIRECTORY });
const folder = create(EntrySchema, {
  entryId: 'fixture-folder', parentId: root.entryId, revision: 'folder-r1', name: 'Field notes', kind: EntryKind.DIRECTORY
});
const file = create(EntrySchema, {
  entryId: 'fixture-file', parentId: folder.entryId, revision: 'file-r1', name: 'report.txt', kind: EntryKind.FILE, sizeBytes: 9n
});

const wire = <Descriptor extends DescMessage>(schema: Descriptor, data?: MessageInitShape<Descriptor>) => ({
  contentType: 'application/proto',
  body: Buffer.from(toBinary(schema, create(schema, data)))
});

async function mockWorkspace(page: Page, baseURL: string) {
  await page.context().addCookies([{
    name: 'arcdos_search_fixture', value: 'test-session', url: baseURL, httpOnly: true, sameSite: 'Lax'
  }]);
  await page.route('**/auth/session', (route) => route.fulfill({
    json: { mode: 'oidc', user: { name: 'Test member', roles: ['drive-users'] }, csrf_token: csrf }
  }));
  await page.route('**/arcdos.files.v1.FileService/GetEntry', (route) => {
    const request = fromBinary(GetEntryRequestSchema, route.request().postDataBuffer() ?? Buffer.alloc(0));
    return route.fulfill(wire(GetEntryResponseSchema, { entry: request.entryId === folder.entryId ? folder : root }));
  });
  await page.route('**/arcdos.files.v1.FileService/ListEntries', (route) => route.fulfill(wire(ListEntriesResponseSchema)));
  await page.route('**/arcdos.files.v1.FileService/ListChanges', (route) => route.fulfill(wire(ListChangesResponseSchema)));
  await page.route('**/api/files/v1/fixture-file/content', (route) => route.fulfill({
    body: 'synthetic', contentType: 'text/plain'
  }));
}

test('searches the shared drive, previews files, edits tags, and opens folders', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('The Playwright base URL is required.');
  await mockWorkspace(page, baseURL);
  await page.route(searchRpc, async (route) => {
    const request = fromBinary(SearchFilesRequestSchema, route.request().postDataBuffer() ?? Buffer.alloc(0));
    const headers = await route.request().allHeaders();
    expect(headers.cookie).toContain('arcdos_search_fixture=test-session');
    expect(headers['x-csrf-token']).toBe(csrf);
    expect(headers.authorization).toBeUndefined();
    expect(request.query).toBe('field');
    await route.fulfill(wire(SearchFilesResponseSchema, {
      hits: [
        { entry: file, path: 'Field notes/report.txt', tags: ['sample'] },
        { entry: folder, path: 'Field notes' }
      ],
      hasMore: true
    }));
  });
  await page.route(tagsRpc, async (route) => {
    const request = fromBinary(SetTagsRequestSchema, route.request().postDataBuffer() ?? Buffer.alloc(0));
    expect(request).toMatchObject({ entryId: file.entryId, expectedRevision: file.revision, tags: ['review', '2026, Q4'] });
    expect((await route.request().allHeaders())['x-csrf-token']).toBe(csrf);
    await route.fulfill(wire(SetTagsResponseSchema, { tags: request.tags }));
  });

  await page.goto('/files/');
  await expect(page.getByRole('heading', { name: 'My files' })).toBeVisible();
  const search = page.getByRole('searchbox', { name: 'Search files' });
  await page.keyboard.press('Control+k');
  await expect(search).toBeFocused();
  await search.fill(' field ');
  await expect(page.getByRole('heading', { name: 'Search results' })).toBeVisible();
  await expect(page.getByText('2 matches shown. Refine your search to find more.')).toBeVisible();
  await expect(page.getByText('Field notes/report.txt', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'report.txt', exact: true }).click();
  const preview = page.getByRole('dialog', { name: 'report.txt' });
  await expect(preview.getByLabel('File contents')).toHaveText('synthetic');
  await preview.getByRole('button', { name: 'Close preview' }).click();

  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Edit tags for report.txt' }).click();
  const editor = page.getByRole('textbox', { name: 'Tags for report.txt' });
  await expect(editor).toBeFocused();
  await expect(editor).toHaveValue('sample');
  await editor.fill('review\n2026, Q4\nreview');
  await page.getByRole('button', { name: 'Save tags' }).click();
  await expect(page.getByText('Tags review · 2026, Q4')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.getByRole('button', { name: 'Field notes', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Field notes' })).toBeVisible();
  await expect(page).toHaveURL(/folder=fixture-folder$/);
  await expect(search).toHaveValue('');
  await expect(page.getByRole('searchbox', { name: 'Filter files' })).toBeVisible();
});

test('cancels stale search results when the query changes and returns to browsing', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('The Playwright base URL is required.');
  await mockWorkspace(page, baseURL);
  const oldResponse = Promise.withResolvers<void>();
  const oldRequestStarted = Promise.withResolvers<void>();
  const oldRequestFinished = Promise.withResolvers<void>();
  await page.route(searchRpc, async (route) => {
    const request = fromBinary(SearchFilesRequestSchema, route.request().postDataBuffer() ?? Buffer.alloc(0));
    if (request.query === 'old') {
      oldRequestStarted.resolve();
      await oldResponse.promise;
      try {
        await route.fulfill(wire(SearchFilesResponseSchema, { hits: [{ entry: file, path: 'old/report.txt' }] }));
      } catch {
        // Browsers can close an aborted request before Playwright sends its mock response.
      } finally {
        oldRequestFinished.resolve();
      }
      return;
    }
    expect(request.query).toBe('new');
    await route.fulfill(wire(SearchFilesResponseSchema));
  });

  await page.goto('/files/');
  const search = page.getByRole('searchbox', { name: 'Search files' });
  try {
    await search.fill('old');
    await oldRequestStarted.promise;
    await search.fill('new');
    await expect(page.getByText('No matching files or folders.')).toBeVisible();
  } finally {
    oldResponse.resolve();
  }
  await oldRequestFinished.promise;
  await expect(page.getByText('No matching files or folders.')).toBeVisible();
  await expect(page.getByText('old/report.txt')).toHaveCount(0);
  await page.getByRole('button', { name: 'Clear search' }).click();
  await expect(page.getByRole('heading', { name: 'My files' })).toBeVisible();
  await expect(search).toBeFocused();
  await expect(search).toHaveValue('');
});
