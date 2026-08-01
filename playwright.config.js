const { defineConfig, devices } = require('@playwright/test');

const externallyManaged = Boolean(process.env.PLAYWRIGHT_BASE_URL);
const baseURL = process.env.PLAYWRIGHT_BASE_URL || 'http://127.0.0.1:8093';

module.exports = defineConfig({
  testDir: './tests/e2e',
  workers: 1,
  fullyParallel: false,
  timeout: 45_000,
  expect: { timeout: 10_000 },
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure'
  },
  projects: [
    { name: 'chromium-desktop', use: { ...devices['Desktop Chrome'], viewport: { width: 1280, height: 900 } } },
    {
      name: 'chromium-mobile',
      testIgnore: /live-sync\.spec\.js/,
      use: { browserName: 'chromium', viewport: { width: 420, height: 800 } }
    }
  ],
  webServer: externallyManaged ? undefined : {
    command: './scripts/build-web.sh --skip-checks --serverless-local && python3 -m http.server 8093 --directory apps/web',
    url: 'http://127.0.0.1:8093',
    reuseExistingServer: false,
    timeout: 180_000
  }
});
