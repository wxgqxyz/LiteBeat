import { expect, test } from '@playwright/test';

// 这些用例断言的是「只有一个播放器且它跨页面活着」这类真机行为：
// player-state 变成 playing 只能由 <audio> 真的解码并播放成功触发，mock 状态骗不过它。
// 前置条件见 web/playwright.config.ts（后端已起、曲库至少两首）。

test('页面切换保留唯一播放器', async ({ page }) => {
  await page.goto('/library');
  await page.getByTestId('track-play').first().click();
  await expect(page.getByTestId('player-state')).toHaveText('playing');
  await page.getByTestId('nav-albums').click();
  await expect(page.locator('audio')).toHaveCount(1);
  await expect(page.getByTestId('player-state')).toHaveText('playing');
});

test('切到搜索页再切回来，同一个音频元素继续播放', async ({ page }) => {
  await page.goto('/library');
  await page.getByTestId('track-play').first().click();
  await expect(page.getByTestId('player-state')).toHaveText('playing');
  const srcBefore = await page.locator('audio').first().getAttribute('src');

  await page.getByTestId('nav-search').click();
  await page.getByTestId('nav-library').click();

  await expect(page.locator('audio')).toHaveCount(1);
  await expect(page.getByTestId('player-state')).toHaveText('playing');
  expect(await page.locator('audio').first().getAttribute('src')).toBe(srcBefore);
  await expect
    .poll(() => page.locator('audio').first().evaluate((el) => el.currentTime))
    .toBeGreaterThan(0);
});

test('刷新恢复队列与位置，但不自动播放', async ({ page }) => {
  await page.goto('/library');
  await page.getByTestId('track-play').first().click();
  await expect(page.getByTestId('player-state')).toHaveText('playing');
  // 位置写盘节流到 2 秒一次，先真的播过这个窗口再去刷新。
  await expect
    .poll(() => page.locator('audio').first().evaluate((el) => el.currentTime), { timeout: 10_000 })
    .toBeGreaterThan(2.5);
  await page.getByTestId('queue-open').click();
  await expect(page.getByTestId('queue-item')).not.toHaveCount(0);

  await page.reload();

  await expect(page.locator('audio')).toHaveCount(1);
  // 恢复后停在准备态：位置已落位，但是否发声由用户点。
  await expect(page.getByTestId('player-state')).toHaveText(/paused|buffering|loading|idle/);
  await expect(page.getByTestId('player-state')).not.toHaveText('playing', { timeout: 3_000 });
  const restored = await page.locator('audio').first().evaluate((el) => el.currentTime);
  expect(restored).toBeGreaterThan(1.5);
  // 滑块 value 必须跟着恢复出的位置走：曾因为「位置先到、时长后到」被 max 夹死在旧上限。
  const sliderValue = await page
    .getByTestId('player-seek')
    .evaluate((el) => Number((el as HTMLInputElement).value));
  expect(Math.abs(sliderValue - restored)).toBeLessThanOrEqual(2);
  await page.getByTestId('queue-open').click();
  await expect(page.getByTestId('queue-item').first()).toBeVisible({ timeout: 5_000 });
});

test('中文关键词搜索命中后可以播放', async ({ page }) => {
  await page.goto('/search');
  await page.getByTestId('search-input').fill('中文');
  const firstRow = page.getByTestId('track-play').first();
  await expect(firstRow).toBeEnabled({ timeout: 5_000 });
  await firstRow.click();
  await expect(page.getByTestId('player-state')).toHaveText('playing');
});

test('队列可以移除条目并清空', async ({ page }) => {
  await page.goto('/library');
  const rows = page.getByTestId('track-play');
  await expect(rows.first()).toBeVisible();
  await rows.nth(0).click();
  await page.getByTestId('track-enqueue').nth(1).click();
  await page.getByTestId('track-enqueue').nth(2).click();
  await page.getByTestId('queue-open').click();
  await expect(page.getByTestId('queue-item')).toHaveCount(3, { timeout: 5_000 });
  await page.getByTestId('queue-remove').first().click();
  await expect(page.getByTestId('queue-item')).toHaveCount(2);
  await page.getByTestId('queue-clear').click();
  await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
