import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';
import svelte from '@astrojs/svelte';
import tailwindcss from '@tailwindcss/vite';
import vlLang from './src/grammars/vl.tmLanguage.json';

// https://astro.build/config
export default defineConfig({
  server: { port: 4335 },
  integrations: [mdx(), svelte()],
  markdown: {
    shikiConfig: {
      // Custom TextMate grammar for the VL language (` ```vl ` fences).
      langs: [vlLang],
      themes: {
        light: 'github-light',
        dark: 'github-dark',
      },
    },
  },
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
