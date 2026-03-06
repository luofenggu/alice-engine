// @ts-check
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');

const AUTH_SECRET = process.env.AUTH_SECRET || 'test-secret';
const INSTANCES_DIR = process.env.INSTANCES_DIR;
const MOCK_LLM_PORT = process.env.MOCK_LLM_PORT;

test('Capture async — summary triggers background capture and updates knowledge', async ({ page, context }) => {
  if (!INSTANCES_DIR) throw new Error('INSTANCES_DIR env not set');
  if (!MOCK_LLM_PORT) throw new Error('MOCK_LLM_PORT env not set');

  await page.goto('/login');
  await page.fill('#secretInput', AUTH_SECRET);
  await page.click('#loginBtn');
  await page.waitForSelector('#instanceList', { timeout: 10000 });

  // Get existing instance IDs before creating new one
  const beforeIds = await page.evaluate(async () => {
    const res = await fetch('api/instances');
    const instances = await res.json();
    return instances.map(i => i.id);
  });

  const newBtn = page.locator('.sidebar button', { hasText: '+ New' });
  await newBtn.click();
  await page.waitForSelector('#chatInput', { state: 'visible', timeout: 15000 });

  // Get instance IDs after creating new one, find the new one
  const instanceId = await page.evaluate(async (before) => {
    const res = await fetch('api/instances');
    const instances = await res.json();
    const newOnes = instances.filter(i => !before.includes(i.id));
    return newOnes.length > 0 ? newOnes[0].id : instances[instances.length - 1].id;
  }, beforeIds);

  console.log(`[capture_async] Using instance: ${instanceId}`);

  await page.fill('#msgInput', '请验证 capture async');
  await page.keyboard.press('Enter');

  // Wait for session block to be written (proves summary executed)
  const sessionsDir = path.join(INSTANCES_DIR, instanceId, 'memory', 'sessions');
  await expect.poll(() => {
    try {
      return fs.readdirSync(sessionsDir).filter(f => f.endsWith('.jsonl')).length;
    } catch (_) {
      return 0;
    }
  }, {
    timeout: 30000,
    intervals: [500, 1000, 2000]
  }).toBeGreaterThan(0);

  // Wait for knowledge to be updated by capture (proves async capture worked)
  const knowledgePath = path.join(INSTANCES_DIR, instanceId, 'memory', 'knowledge.md');
  await expect.poll(() => {
    try {
      return fs.readFileSync(knowledgePath, 'utf-8');
    } catch (_) {
      return '';
    }
  }, {
    timeout: 30000,
    intervals: [500, 1000, 2000]
  }).toContain('CAPTURE_KNOWLEDGE_OK');

  // Verify mock received at least 2 requests (beat + capture)
  const statsResp = await context.request.get(`http://127.0.0.1:${MOCK_LLM_PORT}/stats`);
  expect(statsResp.ok()).toBeTruthy();
  const stats = await statsResp.json();
  expect(stats.total_requests).toBeGreaterThanOrEqual(2);

  console.log('[capture_async] All assertions passed');
});
