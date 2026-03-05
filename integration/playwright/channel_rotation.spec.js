// @ts-check
const { test, expect } = require('@playwright/test');

const AUTH_SECRET = process.env.AUTH_SECRET || 'test-secret';
const MOCK_LLM_PORT = process.env.MOCK_LLM_PORT;

test('Channel rotation — failover from primary to extra channel', async ({ page }) => {
  const mockBaseURL = `http://127.0.0.1:${MOCK_LLM_PORT}`;

  // === Step 1: Login ===
  await page.goto('/login');
  await page.fill('#secretInput', AUTH_SECRET);
  await page.click('#loginBtn');
  await page.waitForSelector('#instanceList', { timeout: 10000 });
  console.log('✅ Login successful');

  // === Step 2: Set extra_channels via global settings API ===
  // Use page.evaluate to share browser cookies (avoid manual cookie handling)
  const mockModel = `${mockBaseURL}/v1/chat/completions@model-B`;
  const settingsOk = await page.evaluate(async (model) => {
    const resp = await fetch('api/settings', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        extra_channels: [
          { api_key: 'test-api-key-b', model: model }
        ]
      })
    });
    return resp.ok;
  }, mockModel);
  expect(settingsOk).toBeTruthy();
  console.log('✅ Extra channels configured');

  // === Step 3: Create instance ===
  const newBtn = page.locator('.sidebar button', { hasText: '+ New' });
  await newBtn.click();
  await page.waitForSelector('#chatInput', { state: 'visible', timeout: 15000 });
  console.log('✅ Instance created');

  // === Step 4: Send message (triggers inference) ===
  await page.fill('#msgInput', 'Test channel rotation');
  await page.click('.send-btn');
  console.log('✅ Message sent — primary channel should get 402, then rotate to extra');

  // === Step 5: Wait for agent reply from channel B ===
  // First request returns 402 → engine shows error message → 10s backoff → retry with channel B
  // We need to wait for the SUCCESS reply, not the error message
  const successReply = page.locator('#chatMessages .msg.agent', { hasText: 'Channel rotation works!' });
  await expect(successReply).toBeVisible({ timeout: 45000 });

  const replyText = await successReply.textContent();
  console.log(`Agent reply: ${replyText}`);
  console.log('✅ Channel rotation verified — response came from channel B');

  // === Step 6: Verify mock server stats ===
  // Use Playwright's request API (not page.evaluate) to avoid CORS restrictions
  const statsResp = await page.request.get(`${mockBaseURL}/stats`);
  const statsJson = await statsResp.json();
  console.log(`Mock server stats: ${JSON.stringify(statsJson)}`);

  // Should have received requests with both model names
  const models = statsJson.models;
  expect(models.length).toBeGreaterThanOrEqual(2);
  // First request should be the primary model, second should be model-B after rotation
  const hasModelB = models.some(m => m.includes('model-B'));
  expect(hasModelB).toBeTruthy();
  console.log('✅ Stats confirmed: both channels were tried');
});

