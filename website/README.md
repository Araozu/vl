# VL docs site

Astro + Tailwind v4 + Svelte. Swift-docs-style dark theme, Inter, dark only.
Landing page is the only `.astro` content page; everything under
`src/pages/docs/` and `src/pages/api/` is pure Markdown styled by
`src/layouts/Docs.astro` + `.docs-prose` in `src/styles/global.css`.

```sh
pnpm install
pnpm dev      # http://localhost:4321
pnpm build
```

- `src/pages/index.astro` — landing + Svelte playground stub
- `src/pages/docs/` — overview, getting-started, language, cli (markdown only)
- `src/pages/api/` — overview, driver, frontend, backend (markdown only)
- `src/components/Playground.svelte` — interactive stub (Svelte)
