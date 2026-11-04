import { expect, test, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import type { MockWindow } from './fixtures/window';

declare global {
  interface Window { testWindow: MockWindow }
}

const commandCalls = (page: Page) => page.evaluate(() =>
  window.testWindow.commands.filter(({ command }) =>
    command.startsWith('plugin:window|') && command !== 'plugin:window|is_maximized'));

// The browser mock cannot enforce native ACLs. Check every mutating command
// against the main window's capability as well as testing the rendered controls.
test('main window grants the required window permissions', () => {
  const capability = JSON.parse(readFileSync(new URL('../../capabilities/default.json', import.meta.url), 'utf8'));
  expect(capability.windows).toEqual(['main']);
  for (const command of ['minimize', 'toggle-maximize', 'close', 'start-dragging', 'start-resize-dragging']) {
    expect(capability.permissions).toContain(`core:window:allow-${command}`);
  }
});

test.beforeEach(async ({ page }) => {
  await page.goto('/tests/fixtures/window.html');
  await expect(page.getByRole('button', { name: 'Maximize window', exact: true })).toBeVisible();
});

test('title is draggable while controls are separate and keyboard accessible', async ({ page }) => {
  await expect(page.locator('[data-tauri-drag-region] h1')).toHaveText('Sideboat');
  await expect(page.locator('.window-controls [data-tauri-drag-region]')).toHaveCount(0);
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Minimize window', exact: true })).toBeFocused();
  await page.keyboard.press('Enter');
  await page.getByRole('button', { name: 'Close window', exact: true }).click();
  expect(await commandCalls(page)).toEqual([
    { command: 'plugin:window|minimize', args: { label: 'main' } },
    { command: 'plugin:window|close', args: { label: 'main' } },
  ]);
});

test('maximize and restore update the icon, label and resize handles', async ({ page }) => {
  await page.getByRole('button', { name: 'Maximize window', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Restore window', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Resize window', exact: true })).toHaveCount(0);
  await expect(page.locator('.window-resize-edge')).toHaveCount(0);
  await page.getByRole('button', { name: 'Restore window', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Maximize window', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Resize window', exact: true })).toBeVisible();
  expect((await commandCalls(page)).map((call) => call.command)).toEqual([
    'plugin:window|toggle_maximize', 'plugin:window|toggle_maximize',
  ]);
});

test('compositor resize events keep maximize state in sync', async ({ page }) => {
  await page.evaluate(() => window.testWindow.resize(true));
  await expect(page.getByRole('button', { name: 'Restore window', exact: true })).toBeVisible();
  await expect(page.locator('.window-resize-grip')).toHaveCount(0);
  await page.evaluate(() => window.testWindow.resize(false));
  await expect(page.getByRole('button', { name: 'Maximize window', exact: true })).toBeVisible();
  await expect(page.locator('.window-resize-grip')).toBeVisible();
});

test('all eight resize directions reach the native API; right click does not resize', async ({ page }) => {
  for (const edge of ['North', 'South', 'East', 'West', 'NorthEast', 'NorthWest', 'SouthWest']) {
    await page.locator(`[data-edge="${edge}"]`).click();
  }
  const grip = page.getByRole('button', { name: 'Resize window', exact: true });
  await grip.click({ button: 'right' });
  expect(await commandCalls(page)).toHaveLength(7);
  await grip.click();
  expect(await commandCalls(page)).toEqual([
    'North', 'South', 'East', 'West', 'NorthEast', 'NorthWest', 'SouthWest', 'SouthEast',
  ].map((value) => ({ command: 'plugin:window|start_resize_dragging', args: { label: 'main', value } })));
});

for (const viewport of [{ width: 1180, height: 780 }, { width: 880, height: 620 }]) {
  test(`resize grip is visible without hover at ${viewport.width}x${viewport.height}`, async ({ page }, testInfo) => {
    await page.setViewportSize(viewport);
    await page.mouse.move(200, 200);
    const grip = page.getByRole('button', { name: 'Resize window', exact: true });
    await expect(grip).toBeInViewport({ ratio: 1 });
    await expect(grip.locator('svg')).toBeVisible();
    expect(await grip.boundingBox()).toEqual({ x: viewport.width - 28, y: viewport.height - 28, width: 28, height: 28 });
    const style = await grip.locator('svg').evaluate((icon) => {
      const css = getComputedStyle(icon);
      return { stroke: css.stroke, width: css.strokeWidth, opacity: css.opacity };
    });
    expect(style).toEqual({ stroke: 'rgb(240, 246, 255)', width: '2px', opacity: '1' });
    const screenshot = testInfo.outputPath('window-controls.png');
    await page.screenshot({ path: screenshot });
    await testInfo.attach('window-controls', { path: screenshot, contentType: 'image/png' });
  });
}

test('native failures are reported', async ({ page }) => {
  await page.evaluate(() => { window.testWindow.failCommand = 'plugin:window|minimize'; });
  await page.getByRole('button', { name: 'Minimize window', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Window action denied');
});
