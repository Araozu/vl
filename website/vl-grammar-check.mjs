import { readFileSync, readdirSync } from 'node:fs';
import { createHighlighter } from './node_modules/.pnpm/shiki@4.4.3/node_modules/shiki/dist/index.mjs';

const grammar = JSON.parse(readFileSync('./src/grammars/vl.tmLanguage.json', 'utf8'));

const highlighter = await createHighlighter({
  langs: [grammar],
  themes: ['github-light', 'github-dark'],
});
console.log('loaded languages:', highlighter.getLoadedLanguages().filter((l) => l === 'vl'));

const files = readdirSync('../examples').filter((f) => f.endsWith('.vl')).sort();
let failures = 0;
const check = (file, html, needle, label) => {
  if (!html.includes(needle)) {
    console.error(`FAIL ${file}: missing ${label} (${needle})`);
    failures += 1;
  }
};

for (const file of files) {
  const src = readFileSync(`../examples/${file}`, 'utf8');
  const html = highlighter.codeToHtml(src, { lang: 'vl', theme: 'github-light' });
  const spanCount = (html.match(/<span/g) || []).length;
  console.log(`${file}: ${src.length} chars -> ${spanCount} spans`);
  if (spanCount === 0) {
    console.error(`FAIL ${file}: no highlighted spans`);
    failures += 1;
  }
}

// Spot-check scopes on a representative sample via themed colors:
// github-light: keyword #D73A49, string #032F62, number #005CC5,
// comment #6A737D, function/entity #6F42C1.
const sample = readFileSync('../examples/scalars.vl', 'utf8');
const html = highlighter.codeToHtml(sample, { lang: 'vl', theme: 'github-light' });
check('scalars.vl', html, '#D73A49', 'keyword color (let/function/if)');
check('scalars.vl', html, '#005CC5', 'number/boolean color');
check('scalars.vl', html, '#6A737D', 'comment color');
check('scalars.vl', html, '#6F42C1', 'function-name color');

const hello = readFileSync('../examples/hello.vl', 'utf8');
const helloHtml = highlighter.codeToHtml(hello, { lang: 'vl', theme: 'github-light' });
check('hello.vl', helloHtml, '#032F62', 'string color');

// Unterminated strings and bad escapes must not throw / hang.
for (const bad of ['"not closed\nlet x = 1;', '"bad\\q"', 'let x = 1.5;']) {
  highlighter.codeToHtml(bad, { lang: 'vl', theme: 'github-light' });
}
console.log('edge cases ok');

// Dual-theme output carries dark variants for the media-query CSS.
const dual = highlighter.codeToHtml(sample, { lang: 'vl', themes: { light: 'github-light', dark: 'github-dark' } });
if (!dual.includes('--shiki-dark')) {
  console.error('FAIL: dual-theme output missing --shiki-dark vars');
  failures += 1;
} else {
  console.log('dual-theme vars ok');
}

if (failures > 0) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log('all grammar checks passed');
highlighter.dispose();
