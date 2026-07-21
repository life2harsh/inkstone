const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests/e2e',
  workers: 1,
  fullyParallel: false,
  timeout: 45_000,
  expect: { timeout: 10_000 },
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:8093',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure'
  },
  projects: [
    { name: 'chromium-desktop', use: { ...devices['Desktop Chrome'], viewport: { width: 1280, height: 900 } } },
    { name: 'chromium-mobile', use: { browserName: 'chromium', viewport: { width: 420, height: 800 } } }
  ],
  webServer: {
    command: 'python3 -m http.server 8093 --directory apps/web',
    url: 'http://127.0.0.1:8093',
    reuseExistingServer: false
  }
});
