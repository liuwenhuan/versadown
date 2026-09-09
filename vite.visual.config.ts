import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'visual-dist',
    emptyOutDir: true,
    rollupOptions: {
      input: {
        'cloud-authorization': resolve(__dirname, 'visual-tests/cloud-authorization.html'),
        'download-queue': resolve(__dirname, 'visual-tests/download-queue.html')
      }
    }
  }
});
