---
layout: ../../layouts/Docs.astro
title: Language tour · VL
---

<h1>Language tour</h1>
<p>A sketch of the v0 surface. For now, every value is an <code>int</code>.</p>

<h2>The whole of it, nearly</h2>
<pre>let x = 1 + 2 * 3;
function main() &#123; let d = x - 1; d; &#125;</pre>
<p class="mt-4">Integers with <code>+ - * /</code>, unary minus, and parentheses. <code>let</code> binds a name, <code>function</code> takes parameters, <code>//</code> starts a comment that runs to the line end. Every statement ends with <code>;</code>.</p>

<h2>What the compiler promises</h2>
<p>Scopes reject two things: names nobody defined, and names defined twice. After an error the compiler marks its nodes and stays quiet downstream, so you fix causes, not echoes.</p>

<h2>Still to write</h2>
<ul class="list-disc space-y-1 pl-6">
  <li>The grammar, with precedence</li>
  <li>Scoping rules, stated precisely</li>
  <li>Recovery and poisoning, with examples</li>
  <li>What comes next: <code>bool</code>, <code>string</code>, function types</li>
</ul>
