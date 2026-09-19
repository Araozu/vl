// Progressive enhancement for VL code snippets.
//
// Finds every Shiki-highlighted `vl` plate (`pre[data-language="vl"]`,
// rendered both by Astro Markdown fences and `<Code>` plus the
// YAML-generated std pages) and adds a little Run button that does the same
// thing as the index Playground: compile via the hosted compiler service and
// show the build output inline. VM execution stays stubbed — see vl-compile.ts.

import { compileVl, formatCompileLines } from './vl-compile';

function enhance(root: ParentNode) {
  const blocks = root.querySelectorAll('pre[data-language="vl"]');
  blocks.forEach((pre) => {
    if (!(pre instanceof HTMLElement)) return;
    if (pre.parentElement?.classList.contains('vl-snippet')) return;

    const wrapper = document.createElement('div');
    wrapper.className = 'vl-snippet';
    pre.parentNode?.insertBefore(wrapper, pre);
    wrapper.appendChild(pre);

    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'vl-run-btn';
    button.textContent = 'Run';
    button.setAttribute('aria-label', 'Run this VL snippet');
    wrapper.appendChild(button);

    const output = document.createElement('div');
    output.className = 'vl-run-output';
    output.hidden = true;
    wrapper.appendChild(output);

    const renderLines = (lines: string[]) => {
      output.hidden = false;
      output.innerHTML = '';
      for (const line of lines) {
        const p = document.createElement('p');
        p.textContent = line;
        output.appendChild(p);
      }
    };

    button.addEventListener('click', async () => {
      const source = pre.innerText.replace(/\n$/, '');
      button.disabled = true;
      button.textContent = 'Building…';
      renderLines(['// compiling…']);
      try {
        const result = await compileVl(source);
        renderLines(formatCompileLines(result));
      } catch (error) {
        renderLines([
          `compiler unavailable — ${error instanceof Error ? error.message : 'request failed'}`,
        ]);
      } finally {
        button.disabled = false;
        button.textContent = 'Run';
      }
    });
  });
}

function init() {
  enhance(document);
}

// Astro is MPA without view transitions here, so DOMContentLoaded covers
// first paint; `astro:page-load` covers client-side navigations if enabled.
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init, { once: true });
} else {
  init();
}
document.addEventListener('astro:page-load', () => enhance(document));
