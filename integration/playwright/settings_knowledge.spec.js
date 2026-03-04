// @ts-check
const { test, expect } = require('@playwright/test');

const AUTH_SECRET = process.env.AUTH_SECRET || 'test-secret';

test('Settings, Privilege, and Knowledge — end-to-end', async ({ page, context }) => {
  // === Step 1: Login ===
  await page.goto('/login');
  await page.fill('#secretInput', AUTH_SECRET);
  await page.click('#loginBtn');
  await page.waitForSelector('#instanceList', { timeout: 10000 });
  console.log('✅ Login successful');

  // === Step 2: Create instance ===
  const newBtn = page.locator('.sidebar button', { hasText: '+ New' });
  await newBtn.click();

  // Wait for chat input to become visible (instance born)
  await page.waitForSelector('#chatInput', { state: 'visible', timeout: 15000 });
  console.log('✅ Instance created and selected');

  // Remember current instance ID from chat title
  const chatTitle = await page.locator('#chatTitle').textContent();
  console.log(`Current instance: ${chatTitle}`);

  // === Step 3: Test Privileged toggle ===
  await page.click('button.ops-toggle');
  await page.waitForSelector('#opsMenu.open', { timeout: 5000 });
  console.log('✅ Ops menu opened');

  const privBtn = page.locator('#privilegedBtn');
  await expect(privBtn).toContainText('Off');
  console.log('✅ Privileged initial state: Off');

  // Toggle On (closes ops menu)
  await privBtn.click();
  await page.click('button.ops-toggle');
  await page.waitForSelector('#opsMenu.open', { timeout: 5000 });
  await expect(privBtn).toContainText('On', { timeout: 5000 });
  console.log('✅ Privileged toggled to On');

  // Toggle Off (closes ops menu)
  await privBtn.click();
  await page.click('button.ops-toggle');
  await page.waitForSelector('#opsMenu.open', { timeout: 5000 });
  await expect(privBtn).toContainText('Off', { timeout: 5000 });
  console.log('✅ Privileged toggled back to Off');

  // === Step 4: Test Settings — View ===
  const settingsBtn = page.locator('#opsMenu button', { hasText: 'Settings' });
  await settingsBtn.click();
  await page.waitForSelector('#settingsOverlay', { state: 'visible', timeout: 5000 });
  console.log('✅ Settings panel opened');

  await expect(page.locator('#setName')).toBeVisible();
  await expect(page.locator('#setAvatar')).toBeVisible();
  await expect(page.locator('#setColor')).toBeVisible();
  await expect(page.locator('#setApiKey')).toBeVisible();
  await expect(page.locator('#setModel')).toBeVisible();
  await expect(page.locator('#setPrivileged')).toBeVisible();
  console.log('✅ All settings fields visible');

  // === Step 5: Test Settings — Modify and Save ===
  await page.fill('#setName', 'Test Bot');
  await page.fill('#setAvatar', '🧪');
  await page.fill('#setColor', '#FF6B6B');

  const saveBtn = page.locator('#settingsOverlay button.primary', { hasText: 'Save' });
  await saveBtn.click();
  await page.waitForSelector('#settingsOverlay', { state: 'hidden', timeout: 5000 });
  console.log('✅ Settings saved');

  // Verify name updated in sidebar — find any instance with "Test Bot"
  const testBotName = page.locator('.sidebar .instance-item .instance-name', { hasText: 'Test Bot' });
  await expect(testBotName).toBeVisible({ timeout: 5000 });
  console.log('✅ Sidebar shows "Test Bot"');

  // Verify chat title updated
  await expect(page.locator('#chatTitle')).toContainText('Test Bot', { timeout: 5000 });
  console.log('✅ Chat title updated to "Test Bot"');

  // Re-open settings to verify persistence
  await page.click('button.ops-toggle');
  await page.waitForSelector('#opsMenu.open', { timeout: 5000 });
  await page.locator('#opsMenu button', { hasText: 'Settings' }).click();
  await page.waitForSelector('#settingsOverlay', { state: 'visible', timeout: 5000 });

  const nameVal = await page.locator('#setName').inputValue();
  expect(nameVal).toBe('Test Bot');
  const avatarVal = await page.locator('#setAvatar').inputValue();
  expect(avatarVal).toBe('🧪');
  console.log('✅ Settings values persisted correctly');

  // Close settings
  await page.locator('#settingsOverlay button', { hasText: 'Cancel' }).click();
  await page.waitForSelector('#settingsOverlay', { state: 'hidden', timeout: 5000 });

  // === Step 6: Prepare knowledge content ===
  // Get instance ID from API — find the "Test Bot" instance specifically
  const instances = await page.evaluate(async () => {
    const res = await fetch('api/instances');
    return res.json();
  });
  const testBotInstance = instances.find(i => i.name === 'Test Bot');
  if (!testBotInstance) throw new Error(`Test Bot instance not found. Available: ${JSON.stringify(instances.map(i => ({id: i.id, name: i.name})))}`);
  const instanceId = testBotInstance.id;
  console.log(`Instance ID: ${instanceId} (found ${instances.length} instances total)`);

  // Write test knowledge file directly to instance directory
  const fs = require('fs');
  const path = require('path');
  const instancesDir = process.env.INSTANCES_DIR;
  if (!instancesDir) throw new Error('INSTANCES_DIR env not set');
  const knowledgePath = path.join(instancesDir, instanceId, 'memory', 'knowledge.md');
  const testKnowledge = '## Test Section\n\nThis is test knowledge content.\n\n## Another Section\n\nMore content here.';
  fs.writeFileSync(knowledgePath, testKnowledge);
  console.log(`✅ Knowledge file written to ${knowledgePath}`);

  // === Step 7: Test Knowledge Page (same-tab navigation) ===
  await page.click('a[href="/knowledge.html"]');
  await page.waitForSelector('#instanceList', { timeout: 10000 });
  console.log('✅ Knowledge page loaded');

  const knowledgeInstances = page.locator('#instanceList .instance-item');
  await expect(knowledgeInstances.first()).toBeVisible({ timeout: 5000 });
  console.log('✅ Knowledge page shows instances');

  // Click the instance we wrote knowledge to (Test Bot)
  const testBotItem = page.locator('#instanceList .instance-item', { hasText: 'Test Bot' });
  await testBotItem.click();

  // Verify knowledge content rendered as k-sections (not empty-state)
  const kSection = page.locator('#contentArea .k-section');
  await expect(kSection.first()).toBeVisible({ timeout: 10000 });
  console.log('✅ Knowledge content rendered as k-section');

  // Verify actual content
  const sectionText = await kSection.first().textContent();
  expect(sectionText).toContain('test knowledge content');
  console.log('✅ Knowledge content matches written file');

  // Verify multiple sections rendered
  const sectionCount = await kSection.count();
  expect(sectionCount).toBeGreaterThanOrEqual(2);
  console.log(`✅ ${sectionCount} knowledge sections rendered`);

  console.log('✅ All tests passed!');
});
