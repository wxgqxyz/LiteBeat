import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { request } from '@playwright/test';

// 登录态由后端真实签发：e2e 不伪造 Cookie，否则 CSRF 与会话过期路径就测不到了。
const API = process.env.LITEBEAT_E2E_API ?? 'http://127.0.0.1:8090';
const USER = process.env.LITEBEAT_E2E_USER ?? '';
const PASSWORD = process.env.LITEBEAT_E2E_PASSWORD ?? '';

// 路径按本文件位置解析：npm --prefix 会把工作目录切到 web/，相对路径会写错地方。
const STATE_PATH = fileURLToPath(new URL('./.auth/state.json', import.meta.url));

export default async function globalSetup(): Promise<void> {
  if (!USER || !PASSWORD) {
    throw new Error('缺少 LITEBEAT_E2E_USER / LITEBEAT_E2E_PASSWORD，e2e 无法取得真实会话。');
  }
  const context = await request.newContext({ baseURL: API });
  const response = await context.post('/api/v1/auth/login', {
    data: { username: USER, password: PASSWORD },
  });
  if (!response.ok()) {
    const detail = await response.text();
    await context.dispose();
    throw new Error(`登录失败：HTTP ${response.status()} ${detail}`);
  }
  const state = await context.storageState();
  mkdirSync(dirname(STATE_PATH), { recursive: true });
  writeFileSync(STATE_PATH, JSON.stringify(state));
  await context.dispose();
}
