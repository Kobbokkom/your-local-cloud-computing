import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    assetsDir: 'assets',
    rollupOptions: {
      output: {
        entryFileNames: 'assets/dashboard.js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
  server: {
    host: '127.0.0.1',
    proxy: {
      '/v1': process.env.YLCC_HUB_URL ?? 'http://127.0.0.1:7443',
      '/health': process.env.YLCC_HUB_URL ?? 'http://127.0.0.1:7443',
    },
  },
})