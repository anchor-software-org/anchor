import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  use: {
    baseURL: 'http://127.0.0.1:1421',
    viewport: { width: 1180, height: 780 },
    launchOptions: process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {},
  },
  webServer: {
    command: 'pnpm dev --host 127.0.0.1 --port 1421',
    url: 'http://127.0.0.1:1421',
  },
});
