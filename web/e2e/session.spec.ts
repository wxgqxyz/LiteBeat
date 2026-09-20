import { expect, test, request as apiRequest } from '@playwright/test';

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

// 其余用例靠 storageState 注入会话，没人真正发过一次浏览器登录请求。
// dev 代理改写 Host 导致 CSRF 同源判定失败时，正是这条路径全挂而测试全绿。
test('从表单登录能进入曲库', async ({ page }) => {
  const username = process.env.LITEBEAT_E2E_USER;
  const password = process.env.LITEBEAT_E2E_PASSWORD;
  test.skip(!username || !password, '未设置 LITEBEAT_E2E_USER / LITEBEAT_E2E_PASSWORD');
  await page.goto('/library');
  await page.getByRole('textbox', { name: '用户名' }).fill(username ?? '');
  await page.getByRole('textbox', { name: '口令' }).fill(password ?? '');
  await page.getByRole('button', { name: '登录' }).click();
  await expect(page).toHaveURL(/\/library$/);
  await expect(page.getByTestId('track-play').first()).toBeVisible();
});

// 退出要连带释放音频与受保护状态：只测登录的话，「退出后音频还在播、
// 队列游标还留在本地、旧会话 Cookie 仍能用」这三类问题没人看守。
//
// 这里用 API 登录再把 Cookie 注入浏览器，而不是再填一次表单：后端登录限流是每 IP 每分钟
// 5 次且不可配置，globalSetup 与上一条表单用例已各占一次，同一分钟内连续重跑时第三次表单
// 登录会直接吃到 429「登录尝试过于频繁」，表现为整条 suite 假失败。API 登录走的是同一条
// 真实签发路径，会话保真度不打折。
test('退出登录后音频、本地游标与服务端会话都会失效', async ({ page }) => {
  const username = process.env.LITEBEAT_E2E_USER;
  const password = process.env.LITEBEAT_E2E_PASSWORD;
  test.skip(!username || !password, '未设置 LITEBEAT_E2E_USER / LITEBEAT_E2E_PASSWORD');

  // 本文件整体跑在空会话下，先确认起点是未登录，再注入会话。
  await page.goto('/');
  await expect(page).toHaveURL(/\/login/);

  const api = await apiRequest.newContext({
    baseURL: test.info().project.use.baseURL as string,
  });
  const login = await api.post('/api/v1/auth/login', { data: { username, password } });
  expect(login.status()).toBe(204);
  // 取 API 上下文真实签发的 Cookie，不手工解析 Set-Cookie。
  const { cookies } = await api.storageState();
  expect(cookies.some((c) => c.name === 'lb_session')).toBe(true);
  await page.context().addCookies(cookies);

  await page.goto('/library');
  await page.getByTestId('track-play').first().click();
  await expect(page.getByTestId('player-state')).toHaveText('playing');
  // 游标写盘节流到 2 秒一次，先真的播过这个窗口，才谈得上「清掉了什么」。
  await expect
    .poll(() => page.locator('audio').first().evaluate((el) => el.currentTime), { timeout: 10_000 })
    .toBeGreaterThan(2.5);
  expect(await page.evaluate(() => localStorage.getItem('litebeat.player.v1'))).not.toBeNull();

  await page.getByTestId('sign-out').click();
  await expect(page).toHaveURL(/\/login$/);
  // 闸门回到未登录时不渲染外壳，所以唯一的音频元素应当一起消失。
  await expect(page.locator('audio')).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem('litebeat.player.v1')))
    .toBeNull();

  // 服务端会话也真的作废了，不是只清了前端状态：API 上下文仍带着那份旧 Cookie。
  const probe = await api.get('/api/v1/auth/session');
  expect(probe.status()).toBe(401);
});
