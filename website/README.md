# VL docs site

Astro + Tailwind v4 + Svelte. Swift-docs-style dark theme, Inter, dark only.
Landing page is the only `.astro` content page; the documentation routes are
pure Markdown styled by
`src/layouts/Docs.astro` + `.docs-prose` in `src/styles/global.css`.

```sh
pnpm install
pnpm dev      # http://localhost:4321
pnpm build
```

- `src/pages/index.astro` — landing + Svelte playground
- `src/pages/docs/` — learning path, getting-started, and language guide (markdown only)
- `src/data/stdlib.yaml` + `src/content.config.ts` — stdlib catalog (Zod-validated content collection)
- `src/pages/std/` — generated standard library pages (`[id].astro` per module, `index.astro` overview)
- `src/pages/cli/` — CLI reference
- `src/pages/internals/` — compiler pipeline, crates, driver, and service reference
- `src/components/Playground.svelte` — interactive compiler client (Svelte)
- `src/grammars/vl.tmLanguage.json` — Shiki/TextMate grammar for VL;
  registered as a custom `langs` entry in `astro.config.mjs`, so ` ```vl `
  fences and `<Code lang={vlLang}>` blocks highlight. Dual `github-light` /
  `github-dark` themes; dark-mode swap lives in `src/styles/global.css`.

## Deploy (`+devops/`)

The playground calls the separate `vlc` service at `https://vlc.nara-lang.org`.
Set `PUBLIC_VLC_URL` when building the site to use another compiler endpoint.

Same shape as `nikki.nara-lang.org`: multi-stage Dockerfile (pnpm build →
nginx serves `dist/`), Jenkins pipeline per stage, Ansible to the target host,
Traefik on the `proxy` network terminates TLS for `vl.nara-lang.org`.

```sh
docker build --pull -f +devops/docker/Dockerfile -t vl-docs:local .
docker run --rm -p 8080:80 vl-docs:local
```

Stage config lives in `+devops/+develop/` (`Jenkinsfile`, `inventory.yml`,
`docker-compose.full.yml.j2`); shared playbooks in `+devops/ansible/`.
