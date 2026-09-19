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
- `src/pages/std/` — standard library overview and module references (markdown only)
- `src/pages/cli/` — CLI reference
- `src/pages/internals/` — compiler pipeline, crates, driver, and service reference
- `src/components/Playground.svelte` — interactive compiler client (Svelte)

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
