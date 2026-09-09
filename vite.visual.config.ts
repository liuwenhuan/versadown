import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'visual-dist',
    emptyOutDir: true,
    rollupOptions: {
      input: resolve(__dirname, 'visual-tests/cloud-authorization.html')
    }
  }
});
