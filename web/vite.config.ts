import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// changeOrigin 必须留在 false：后端 CSRF 要求 Origin 与 Host 完全一致，
// 代理若把 Host 改写成 127.0.0.1:8090，浏览器带的 :5173 Origin 会被判 403，dev 模式无法登录。
const backend = { target: 'http://127.0.0.1:8090', changeOrigin: false };

export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      '/api': backend,
      '/media': backend,
      '/health': backend,
    },
  },
});
