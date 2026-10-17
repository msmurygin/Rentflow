import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

// Статика собирается в dist/ и отдаётся Caddy (или самим rentflow через WEB_DIR). Отдельного Node-сервера в production нет.
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  base: './',
  server: { proxy: { '/api': 'http://127.0.0.1:8080' } },
  build: { target: 'es2022', outDir: 'dist', sourcemap: false },
  test: { include: ['src/**/*.test.ts'] },
});
