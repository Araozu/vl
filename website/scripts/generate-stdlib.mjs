import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';
import { mergeCatalog } from './stdlib-catalog.mjs';

const repo = fileURLToPath(new URL('../../', import.meta.url));
// Docker builds export the catalog in the Rust stage. Local commands invoke
// the current compiler, never a checked-in snapshot or a PATH-installed VL.
const api = JSON.parse(process.env.VL_STDLIB_CATALOG
  ? readFileSync(process.env.VL_STDLIB_CATALOG, 'utf8')
  : execFileSync('cargo', ['run', '--quiet', '--locked', '--bin', 'vl', '--', 'stdlib'], {
    cwd: repo, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
  }));
const editorial = parse(readFileSync(new URL('../src/data/stdlib.yaml', import.meta.url), 'utf8'));
const catalog = mergeCatalog(api, editorial);
writeFileSync(new URL('../src/data/stdlib.generated.json', import.meta.url),
  `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`Generated stdlib docs: ${catalog.length} modules, ${catalog.reduce((n, m) => n + m.functions.length, 0)} functions.`);
