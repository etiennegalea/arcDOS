import { expect, test } from '@playwright/test';

const healthEndpoint = '**/arcdos.account.v1.HealthService/Check';

test('checks server health over binary Connect RPC', async ({ page }) => {
  const firstCheck = page.waitForResponse(healthEndpoint);
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Server status' })).toBeVisible();

  const response = await firstCheck;
  expect(response.ok()).toBe(true);
  expect(response.request().method()).toBe('POST');
  expect(response.request().headers()['content-type']).toBe('application/proto');
  expect(response.request().headers()['connect-protocol-version']).toBe('1');
  expect(response.headers()['content-type']).toBe('application/proto');
  await expect(page.getByRole('status')).toContainText('Ready');

  const repeatCheck = page.waitForResponse(healthEndpoint);
  await page.getByRole('button', { name: 'Check again' }).click();
  expect((await repeatCheck).ok()).toBe(true);
  await expect(page.getByRole('status')).toContainText('Ready');
});

test('allows a health check to recover after a network failure', async ({ page }) => {
  await page.route(healthEndpoint, (route) => route.abort(), { times: 1 });
  await page.goto('/');
  await expect(page.getByRole('status')).toContainText('Unavailable');

  await page.getByRole('button', { name: 'Check again' }).click();
  await expect(page.getByRole('status')).toContainText('Ready');
});
