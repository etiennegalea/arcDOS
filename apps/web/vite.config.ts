import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit({ adapter: adapter() })],
  server: {
    proxy: {
      '^/arcdos\\.': process.env.ARCDOS_API_ORIGIN ?? 'http://127.0.0.1:8080',
      '/api/files/v1': process.env.ARCDOS_API_ORIGIN ?? 'http://127.0.0.1:8080'
    }
  }
});
