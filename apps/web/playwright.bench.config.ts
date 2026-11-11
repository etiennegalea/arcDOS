import { defineConfig } from '@playwright/test';
import integration from './playwright.config';

export default defineConfig({
  ...integration,
  testDir: './tests/perf',
  testIgnore: [],
  outputDir: 'test-results/bench-scroll',
  workers: 1,
  timeout: 30 * 60_000,
  use: {
    ...integration.use,
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
    headless: true,
    locale: 'en-US',
    timezoneId: 'UTC'
  }
});
