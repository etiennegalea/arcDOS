import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  use: {
    baseURL: process.env.ARCDOS_E2E_URL ?? 'http://127.0.0.1:8080',
    browserName: 'chromium',
    channel: process.env.ARCDOS_E2E_BROWSER_CHANNEL
  }
});
