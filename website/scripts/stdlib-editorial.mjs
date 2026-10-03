import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parse } from 'yaml';

// One optional editorial document per module, discovered in stable order.
export function loadEditorial(directory) {
  const seen = new Set();
  return readdirSync(directory).filter((name) => /\.ya?ml$/.test(name)).sort().map((name) => {
    const entry = parse(readFileSync(join(directory, name), 'utf8'));
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)
      || typeof entry.module !== 'string') {
      throw new Error(`Invalid stdlib prose in ${name}: expected one module document.`);
    }
    if (seen.has(entry.module)) throw new Error(`Duplicate stdlib prose: module ${entry.module}`);
    seen.add(entry.module);
    return entry;
  });
}
