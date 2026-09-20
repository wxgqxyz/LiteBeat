import { defineConfig, devices } from '@playwright/test';
import { fileURLToPath } from 'node:url';

// e2e 需要三件事先就位：
// 1) 后端已在 LITEBEAT_E2E_API 运行（litebeat serve --config <独立临时数据的配置>）；
// 2) 曲库里至少两首可播曲目，其中一首中文标题；
// 3) LITEBEAT_E2E_USER / LITEBEAT_E2E_PASSWORD 提供真实登录凭据，浏览器已 `playwright install`。
// 前端 dev server 由这里拉起，vite 把 /api、/media、/health 代理到后端。
const BASE_URL = process.env.LITEBEAT_E2E_BASE ?? 'http://127.0.0.1:5173';
const STATE_PATH = fileURLToPath(new URL('./e2e/.auth/state.json', import.meta.url));

export default defineConfig({
  testDir: fileURLToPath(new URL('./e2e', import.meta.url)),
  // 只收 *.spec.ts：Vitest 用例住在 web/tests，命名是 *.test.ts，不能被这里重复加载。
  testMatch: /.*\.spec\.ts$/,
  fullyParallel: false,
  workers: 1,
  reporter: [['list']],
  use: {
    baseURL: BASE_URL,
    storageState: STATE_PATH,
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        // 自带 Chromium 下载不动时，可用 LITEBEAT_E2E_CHANNEL=msedge|chrome 跑系统浏览器。
        channel: process.env.LITEBEAT_E2E_CHANNEL || undefined,
      },
    },
  ],
  globalSetup: fileURLToPath(new URL('./e2e/global-setup.ts', import.meta.url)),
  webServer: {
    command: 'npm run dev -- --port 5173',
    url: BASE_URL,
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
