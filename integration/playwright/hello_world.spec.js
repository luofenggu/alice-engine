// @ts-check
const { test, expect } = require('@playwright/test');

const AUTH_SECRET = process.env.AUTH_SECRET || 'test-secret';

test('Hello World — full end-to-end with browser', async ({ page }) => {
  // === Step 1: Login ===
  await page.goto('/login');
  await page.fill('#secretInput', AUTH_SECRET);
  await page.click('#loginBtn');

  // Wait for redirect to main page (instanceList appears)
  await page.waitForSelector('#instanceList', { timeout: 10000 });
  console.log('✅ Login successful');

  // === Step 2: Create instance ===
  // Click "+ New" button in sidebar footer
  const newBtn = page.locator('.sidebar button', { hasText: '+ New' });
  await newBtn.click();

  // Wait for chat input to become visible (instance selected)
  await page.waitForSelector('#chatInput', { state: 'visible', timeout: 15000 });
  console.log('✅ Instance created and selected');

  // === Step 3: Send message ===
  await page.fill('#msgInput', 'Hello, bot!');
  await page.click('.send-btn');
  console.log('✅ Message sent');

  // === Step 4: Wait for agent reply ===
  // Agent messages have class "msg agent"
  const replyLocator = page.locator('#chatMessages .msg.agent');
  await expect(replyLocator.first()).toBeVisible({ timeout: 30000 });

  // Check the reply content
  const replyText = await replyLocator.first().textContent();
  console.log(`Agent reply: ${replyText}`);
  expect(replyText).toContain('Hello from the Playwright end-to-end test!');
  console.log('✅ Agent reply verified');
});
