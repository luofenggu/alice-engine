// @ts-check
const { test, expect } = require('@playwright/test');

const AUTH_SECRET = process.env.AUTH_SECRET || 'test-secret';

test('Message no duplicate — send message and verify no duplicates', async ({ page }) => {
  // === Step 1: Login ===
  await page.goto('/login');
  await page.fill('#secretInput', AUTH_SECRET);
  await page.click('#loginBtn');
  await page.waitForSelector('#instanceList', { timeout: 10000 });
  console.log('✅ Login successful');

  // === Step 2: Create instance ===
  const newBtn = page.locator('.sidebar button', { hasText: '+ New' });
  await newBtn.click();
  await page.waitForSelector('#chatInput', { state: 'visible', timeout: 15000 });
  console.log('✅ Instance created and selected');

  // === Step 3: Send first message ===
  await page.fill('#msgInput', 'Test message one');
  await page.keyboard.press('Enter');
  console.log('✅ First message sent via Enter key');

  // === Step 4: Verify user message appears immediately (optimistic insert) ===
  const userMsg1 = page.locator('#chatMessages .msg.user', { hasText: 'Test message one' });
  await expect(userMsg1).toBeVisible({ timeout: 5000 });
  console.log('✅ User message displayed immediately');

  // === Step 5: Wait for agent reply ===
  const agentReply1 = page.locator('#chatMessages .msg.agent', { hasText: 'Reply to message one' });
  await expect(agentReply1).toBeVisible({ timeout: 30000 });
  console.log('✅ Agent reply received');

  // === Step 6: Check no duplicate messages ===
  // Count all message elements with data-msg-id
  const allMsgIds = await page.$$eval('#chatMessages .msg[data-msg-id]', els => {
    const ids = els.map(el => el.getAttribute('data-msg-id'));
    return ids;
  });
  console.log(`Message IDs in DOM: ${JSON.stringify(allMsgIds)}`);

  // Check for duplicates
  const uniqueIds = new Set(allMsgIds);
  expect(uniqueIds.size).toBe(allMsgIds.length);
  console.log(`✅ No duplicate messages (${allMsgIds.length} messages, all unique IDs)`);

  // === Step 7: Send second message to verify consistency ===
  await page.fill('#msgInput', 'Test message two');
  await page.keyboard.press('Enter');
  console.log('✅ Second message sent');

  // Wait for second agent reply
  const agentReply2 = page.locator('#chatMessages .msg.agent', { hasText: 'Reply to message two' });
  await expect(agentReply2).toBeVisible({ timeout: 30000 });
  console.log('✅ Second agent reply received');

  // === Step 8: Final duplicate check ===
  const finalMsgIds = await page.$$eval('#chatMessages .msg[data-msg-id]', els => {
    const ids = els.map(el => el.getAttribute('data-msg-id'));
    return ids;
  });
  console.log(`Final message IDs: ${JSON.stringify(finalMsgIds)}`);

  const finalUniqueIds = new Set(finalMsgIds);
  expect(finalUniqueIds.size).toBe(finalMsgIds.length);
  console.log(`✅ No duplicates after second message (${finalMsgIds.length} messages, all unique)`);

  // Verify we have exactly 4 messages (2 user + 2 agent)
  // Note: there might be system/welcome messages, so check >= 4
  expect(finalMsgIds.length).toBeGreaterThanOrEqual(4);
  console.log('✅ All messages accounted for, no duplicates');
});

