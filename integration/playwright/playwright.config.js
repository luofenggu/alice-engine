const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: '.',
  timeout: 60000,
  retries: 0,
  use: {
    baseURL: process.env.ENGINE_URL || 'http://127.0.0.1:9527',
    headless: true,
    screenshot: 'only-on-failure',
  },
  reporter: [['list']],
});
