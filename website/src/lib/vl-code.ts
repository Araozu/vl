import { createHighlighter, type Highlighter } from 'shiki';
import vlLang from '../grammars/vl.tmLanguage.json';

// Highlights ```vl snippets with the same custom TextMate grammar and
// dual light/dark themes wired into Astro's Markdown pipeline
// (`markdown.shikiConfig` in astro.config.mjs), so YAML-generated pages
// render byte-identical code plates to hand-written Markdown pages.
let highlighter: Promise<Highlighter> | undefined;

export function highlightVl(source: string): Promise<string> {
  highlighter ??= createHighlighter({
    langs: [vlLang as never],
    themes: ['github-light', 'github-dark'],
  });
  return highlighter.then((hl) =>
    hl
      .codeToHtml(source.replace(/\n$/, ''), {
        lang: 'vl',
        themes: { light: 'github-light', dark: 'github-dark' },
      })
      // Astro's Markdown pipeline renames Shiki's classes and decorates the
      // <pre> tag; match it exactly so generated plates equal Markdown ones.
      .replace('class="shiki shiki-themes', 'class="astro-code astro-code-themes')
      .replace(
        ';--shiki-dark:#e1e4e8" tabindex="0">',
        ';--shiki-dark:#e1e4e8; overflow-x: auto;" tabindex="0" data-language="vl">',
      ),
  );
}
