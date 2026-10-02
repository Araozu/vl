# VL docs site

Astro + Tailwind v4 + Svelte. Swift-docs-style dark theme, Inter, dark only.
The learning guides are Markdown; the standard library reference uses generated
Astro pages. Both use `src/layouts/Docs.astro` and `.docs-prose` in
`src/styles/global.css`.

```sh
pnpm install
pnpm dev      # http://localhost:4335
pnpm build
```

- `src/pages/index.astro` — landing + Svelte playground
- `src/pages/docs/` — learning path, getting-started, and language guide (markdown only)
- `src/data/stdlib.yaml` — editorial stdlib descriptions and examples; no duplicated signatures
- `scripts/generate-stdlib.mjs` + `src/content.config.ts` — compiler-derived, Zod-validated stdlib catalog
- `src/pages/std/` — generated standard library pages (`[id].astro` per module, `index.astro` overview)
- `src/pages/cli/` — CLI reference
- `src/pages/internals/` — compiler pipeline, crates, driver, and service reference
- `src/components/Playground.svelte` — interactive compiler client (Svelte)
- `src/grammars/vl.tmLanguage.json` — Shiki/TextMate grammar for VL;
  registered as a custom `langs` entry in `astro.config.mjs`, so ` ```vl `
  fences and `<Code lang={vlLang}>` blocks highlight. Dual `github-light` /
  `github-dark` themes; dark-mode swap lives in `src/styles/global.css`.

## Deploy (`+devops/`)

`pnpm dev`, `pnpm check`, and `pnpm build` first run `cargo run --locked --bin vl -- stdlib`
from this checkout. Rust/Cargo is required for local website commands. The command
exports the same merged native/helper catalog used by the frontend, including
generic bounds and error variants. The generator adds optional prose from YAML
and writes ignored `src/data/stdlib.generated.json`; new modules and functions
appear automatically. Removed exports or renamed parameters with stale prose
fail generation so descriptions cannot silently attach to the wrong API.
After changing the crate during a running dev session, run `pnpm docs:generate`
to refresh the reference. `pnpm test` checks the catalog merge behavior.

Docker exports the catalog in a Rust stage and passes it to the website stage
with `VL_STDLIB_CATALOG`, so the final image needs neither Cargo nor the compiler.

The playground compiles via the separate `vlc` service at
`https://vlc.nara-lang.org` and runs the returned vmfile in the browser with
`/naravm.wasm` (host file `/var/bin/naravm-luna-ai.wasm`, see
`+devops/docker/nginx.conf` and `+develop/docker-compose.full.yml.j2`).
Set `PUBLIC_VLC_URL` to use another compiler endpoint and
`PUBLIC_NARAVM_WASM_URL` to use another WASM URL. Local dev needs the artifact:
`cp ~/projects/zig/naravm/zig-out/naravm-wasm32.wasm public/naravm.wasm`.

Same shape as `nikki.nara-lang.org`: multi-stage Dockerfile (Rust catalog → pnpm build →
nginx serves `dist/`), Jenkins pipeline per stage, Ansible to the target host,
Traefik on the `proxy` network terminates TLS for `vl.nara-lang.org`.

```sh
docker build --pull -f website/+devops/docker/Dockerfile -t vl-docs:local . # from repository root
docker run --rm -p 8080:80 vl-docs:local
```

Stage config lives in `+devops/+develop/` (`Jenkinsfile`, `inventory.yml`,
`docker-compose.full.yml.j2`); shared playbooks in `+devops/ansible/`.
