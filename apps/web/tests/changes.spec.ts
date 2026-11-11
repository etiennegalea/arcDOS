import { create, fromBinary, toBinary, type DescMessage, type MessageInitShape } from '@bufbuild/protobuf';
import { expect, test, type Page } from '@playwright/test';
import {
  ChangeKind, EntryKind, GetEntryResponseSchema, ListChangesRequestSchema, ListChangesResponseSchema,
  ListEntriesResponseSchema, ListTrashResponseSchema
} from '../src/lib/gen/arcdos/files/v1/files_pb';

const wire = <Descriptor extends DescMessage>(schema: Descriptor, data?: MessageInitShape<Descriptor>) => ({
  contentType: 'application/proto',
  body: Buffer.from(toBinary(schema, create(schema, data)))
});

async function historicalJournal(page: Page) {
  let release!: () => void;
  const firstPage = new Promise<void>((resolve) => { release = resolve; });
  const cursors: bigint[] = [];
  let changed = false;
  let folderReads = 0;
  let tailSent = false;

  await page.route('**/auth/session', (route) => route.fulfill({
    json: { mode: 'oidc', user: { name: 'Test member', roles: ['drive-users'] }, csrf_token: 'synthetic-csrf' }
  }));
  await page.route('**/arcdos.files.v1.FileService/GetEntry', (route) => {
    folderReads++;
    return route.fulfill(wire(GetEntryResponseSchema, {
      entry: { entryId: 'root', revision: changed ? 'root-after' : 'root-before', kind: EntryKind.DIRECTORY }
    }));
  });
  await page.route('**/arcdos.files.v1.FileService/ListEntries',
    (route) => route.fulfill(wire(ListEntriesResponseSchema)));
  await page.route('**/arcdos.files.v1.FileService/ListTrash',
    (route) => route.fulfill(wire(ListTrashResponseSchema)));
  await page.route('**/arcdos.files.v1.FileService/ListChanges', async (route) => {
    const request = fromBinary(ListChangesRequestSchema, route.request().postDataBuffer() ?? Buffer.alloc(0));
    expect(request.pageSize).toBe(500);
    cursors.push(request.afterSequence);
    if (request.afterSequence === 0n) await firstPage;
    if (request.afterSequence < 1000n) {
      const start = request.afterSequence;
      if (start === 500n) changed = true;
      await route.fulfill(wire(ListChangesResponseSchema, {
        changes: Array.from({ length: 500 }, (_, index) => ({
          sequence: start + BigInt(index + 1), entryId: `historical-${start + BigInt(index)}`, kind: ChangeKind.CREATED
        })),
        nextSequence: start + 500n
      }));
    } else {
      await route.fulfill(wire(ListChangesResponseSchema, { nextSequence: 1000n }));
      tailSent = true;
    }
  });
  return { cursors, release, folderReads: () => folderReads, tailSent: () => tailSent };
}

test('catches up full journal pages and checks the folder at an empty initial tail', async ({ page }) => {
  const journal = await historicalJournal(page);
  await page.goto('/files/');
  await expect(page.getByRole('button', { name: 'New folder' })).toBeEnabled();
  expect(journal.folderReads()).toBe(1);
  journal.release();

  await expect(page.getByRole('button', { name: 'Refresh files' })).toBeVisible();
  expect(journal.cursors.slice(0, 3)).toEqual([0n, 500n, 1000n]);
  expect(journal.folderReads()).toBe(2);
});

for (const collection of ['favorites', 'trash']) {
  test(`initial ${collection} history does not report a new change`, async ({ page }) => {
    const journal = await historicalJournal(page);
    await page.goto(`/files/?view=${collection}`);
    await expect(page.getByRole('heading', {
      name: collection === 'favorites' ? 'Favorites' : 'Trash', exact: true
    })).toBeVisible();
    journal.release();

    await expect.poll(() => journal.cursors.slice(0, 3)).toEqual([0n, 500n, 1000n]);
    await expect.poll(journal.tailSent).toBe(true);
    await page.evaluate(() => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    }));
    await expect(page.getByRole('button', { name: 'Refresh files' })).toHaveCount(0);
    expect(journal.folderReads()).toBe(0);
  });
}
