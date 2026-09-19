// Minimal Markdown-subset renderer for YAML-driven docs prose.
//
// The stdlib catalog stores paragraphs with inline `code` and
// [text](href) links only — no headings, lists, or nested markup.
// Everything renders as plain elements inside `.docs-prose`, so the
// existing stylesheet applies unchanged.

function escapeHtml(text: string): string {
  // Mirrors Astro's Markdown escaping: `&`, `<`, `>` only — quotes stay raw.
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function renderLink(text: string): string {
  return text.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    (_, label: string, href: string) =>
      `<a href="${escapeHtml(href)}">${escapeHtml(label)}</a>`,
  );
}

export function renderInline(text: string): string {
  // Split on backtick pairs so links inside `code` are left alone.
  const parts = text.split('`');
  return parts
    .map((part, i) =>
      i % 2 === 1 ? `<code>${escapeHtml(part)}</code>` : renderLink(escapeHtml(part)),
    )
    .join('');
}

export function renderParagraphs(text: string): string {
  return text
    .split(/\n\s*\n/)
    .map((para) => `<p>${renderInline(para.trim())}</p>`)
    .join('\n');
}

/// GitHub-style slug for heading anchors
/// (`std.print(value: string) -> void` → `stdprintvalue-string-returns-void`).
export function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/->/g, ' returns ')
    .replace(/[^a-z0-9 _-]/g, '')
    .trim()
    .replace(/\s+/g, '-');
}

