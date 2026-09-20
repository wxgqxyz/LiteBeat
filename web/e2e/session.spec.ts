import { expect, test } from '@playwright/test';

// 未登录用例单独一个文件：storageState 置空只对这一组生效，
// 混进 playback.spec.ts 会让已登录的用例一起失去会话。
test.use({ storageState: { cookies: [], origins: [] } });

test('未登录访问曲库会被赶到登录页', async ({ page }) => {
  await page.goto('/library');
  await expect(page).toHaveURL(/\/login\?next=%2Flibrary/);
  await expect(page.getByRole('heading', { name: '管理员登录' })).toBeVisible();
  // 闸门未通过时不应渲染外壳，也就不应存在音频元素。
  await expect(page.locator('audio')).toHaveCount(0);
});

test('登录页直接打开时不追加 next 参数', async ({ page }) => {
  await page.goto('/login');
  await expect(page).toHaveURL(/\/login$/);
  await expect(page.getByRole('heading', { name: '管理员登录' })).toBeVisible();
});
