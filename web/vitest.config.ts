import { defineConfig } from 'vitest/config';

// 单元测试只跑纯逻辑模块（引擎/队列/搜索/分页），不需要 Svelte 编译器参与。
export default defineConfig({
  test: {
    environment: 'jsdom',
    include: ['tests/**/*.test.ts'],
    globals: false,
  },
});
