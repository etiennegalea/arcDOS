import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

const apiOrigin = process.env.ARCDOS_API_ORIGIN ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [sveltekit({ adapter: adapter() })],
  server: {
    proxy: {
      '^/arcdos\\.': apiOrigin,
      '/api/files/v1': apiOrigin,
      '/auth': apiOrigin
    }
  }
});
