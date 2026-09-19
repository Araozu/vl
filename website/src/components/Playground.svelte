<script lang="ts">
  let source = $state('let x = 1 + 2 * 3;\nfunction main() { let d = x - 1; d; }');
  let output = $state<string[]>(['// press Run — this sketch only pretends. The real pipeline lives in the Rust driver.']);

  function run() {
    // Stub: pretend to lex/check. Real playground will call into vl via WASM or server.
    const lines = source.split('\n').filter((l) => l.trim().length > 0);
    output = [
      `ok — ${lines.length} line(s), 0 errors (sketch)`,
      ...lines.map((l, i) => `  L${i + 1}: ${l.trim().slice(0, 60)}`),
      'Tip: cargo run -- check examples/hello.vl runs the real thing.',
    ];
  }

  function loadSample(kind: 'hello' | 'arith' | 'error') {
    if (kind === 'hello') source = 'function main() { 42; }';
    if (kind === 'arith') source = 'let x = 1 + 2 * 3;\nfunction main() { let d = x - 1; d; }';
    if (kind === 'error') source = 'function main() { undefined_var; }';
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
      class="rounded-full bg-accent px-4 py-1 text-[0.82rem] font-medium text-white transition hover:brightness-110"
      onclick={run}
    >
      Run
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
        <p class="text-muted">{line}</p>
      {/each}
    </div>
  </div>
</div>
