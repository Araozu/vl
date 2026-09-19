import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';
import svelte from '@astrojs/svelte';
import tailwindcss from '@tailwindcss/vite';

// https://astro.build/config
export default defineConfig({
  integrations: [mdx(), svelte()],
  redirects: {
    '/docs/cli': '/cli',
    '/api': '/std',
    '/api/std': '/std/std',
    '/api/fs': '/std/fs',
    '/api/string': '/std/string',
    '/api/compiler': '/internals/compiler-service',
    '/api/driver': '/internals/driver',
    '/api/frontend': '/internals/frontend',
    '/api/backend': '/internals/backend',
  },
  vite: {
    plugins: [tailwindcss()],
  },
});
