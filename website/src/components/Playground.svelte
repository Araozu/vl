<script lang="ts">
  const compilerUrl = import.meta.env.PUBLIC_VLC_URL ?? 'https://vlc.nara-lang.org';
  let source = $state('use std.print;\n\nfunction main() {\n    print("Hello, world!\\n");\n}');
  let output = $state<string[]>(['// Naravm compiler ready — press Run']);
  let compiling = $state(false);
  let artifact = $state<Uint8Array | null>(null);

  async function run() {
    compiling = true;
    artifact = null;
    output = ['// compiling on vlc.nara-lang.org…'];
    try {
      const response = await fetch(`${compilerUrl}/v1/compile`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ source, filename: 'playground.vl' }),
      });
      const body = await response.json();
      if (!response.ok || !body.ok) {
        output = [
          `build failed — ${body.error ?? `HTTP ${response.status}`}`,
          ...(body.diagnostics ? body.diagnostics.split('\n') : []),
        ];
        return;
      }
      const bytes = Uint8Array.from(atob(body.bytecode_base64), (char) => char.charCodeAt(0));
      artifact = bytes;
      output = [
        `build ok — ${body.target}, ${bytes.byteLength} byte vmfile`,
        'The artifact is ready for Naravm.',
        ...(body.diagnostics ? body.diagnostics.split('\n') : []),
      ];
    } catch (error) {
      output = [`compiler unavailable — ${error instanceof Error ? error.message : 'request failed'}`];
    } finally {
      compiling = false;
    }
  }

  function download() {
    if (!artifact) return;
    const url = URL.createObjectURL(new Blob([artifact], { type: 'application/octet-stream' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'playground.nara';
    link.click();
    URL.revokeObjectURL(url);
  }

  function loadSample(kind: 'hello' | 'arith' | 'error') {
    if (kind === 'hello') source = 'use std.print;\n\nfunction main() {\n    print("Hello, world!\\n");\n}';
    if (kind === 'arith') source = 'use std.print;\n\nfunction main() {\n    print("2 + 3 = ");\n}';
    if (kind === 'error') source = 'function main() { undefined_var; }';
    artifact = null;
    output = ['// sample loaded — press Run'];
  }
</script>

<div class="overflow-hidden rounded-xl bg-plate text-ink">
  <div class="flex flex-wrap items-center justify-between gap-2 border-b border-rule px-4 py-2.5">
    <div class="flex gap-1 font-mono text-[0.8rem]">
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-pill hover:text-ink" onclick={() => loadSample('hello')}>hello.vl</button>
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-pill hover:text-ink" onclick={() => loadSample('arith')}>arith.vl</button>
      <button class="rounded-md px-2.5 py-1 text-muted transition hover:bg-pill hover:text-ink" onclick={() => loadSample('error')}>err.vl</button>
    </div>
      <button
      class="rounded-full bg-accent px-4 py-1 text-[0.82rem] font-medium text-white transition hover:brightness-110 disabled:cursor-wait disabled:opacity-60"
      onclick={run}
      disabled={compiling}
    >
      {compiling ? 'Building…' : 'Run'}
    </button>
  </div>
  <div class="grid md:grid-cols-2">
    <textarea
      bind:value={source}
      spellcheck={false}
      class="min-h-48 resize-y bg-transparent p-4 font-mono text-[0.83rem] leading-[1.7] text-ink outline-none placeholder:text-faint"
      placeholder="write VL here…"
    ></textarea>
    <div class="border-t border-rule p-4 font-mono text-[0.8rem] leading-[1.7] md:border-l md:border-t-0">
      {#each output as line}
        <p class="whitespace-pre-wrap text-muted">{line}</p>
      {/each}
      {#if artifact}
        <button class="mt-3 rounded-md bg-pill px-2.5 py-1 text-[0.78rem] text-ink transition hover:bg-rule" onclick={download}>
          Download .nara
        </button>
      {/if}
    </div>
  </div>
</div>
