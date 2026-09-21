// Browser runner for Naravm bytecode using the wasm32-freestanding artifact.
//
// The production host carries the Luna WASM build at
// `/var/bin/naravm-luna-ai.wasm` (installed by the naravm `ai-luna/wasm32`
// CI stage). The docs container serves that file as same-origin
// `/naravm.wasm` (see `+devops/`), so the playground can compile via `vlc`
// and then execute the returned vmfile entirely in the browser.
//
// WASM ABI (from `naravm/lib/wasm32.zig`):
//   imports: env.host_stdout_write(ptr, len), env.host_stderr_write(ptr, len)
//   exports: memory, alloc_input(len) -> ptr, free_input(ptr, len),
//            run_bytecode(ptr, len) -> u32 status,
//            get_last_error_ptr() -> ptr, get_last_error_len() -> len
// Status: 0 ok, 1 invalid_input, 2 manager_error, 3 entrypoint_error,
//         4 init_error, 5 runtime_error.

export interface NaravmRunResult {
  status: number;
  statusName: string;
  stdout: string;
  stderr: string;
  error: string;
}

const STATUS_NAMES = [
  'ok',
  'invalid_input',
  'manager_error',
  'entrypoint_error',
  'init_error',
  'runtime_error',
] as const;

type WasmExports = {
  memory: WebAssembly.Memory;
  alloc_input: (len: number) => number;
  free_input: (ptr: number, len: number) => void;
  run_bytecode: (ptr: number, len: number) => number;
  get_last_error_ptr: () => number;
  get_last_error_len: () => number;
};

export function naravmWasmUrl(): string {
  return import.meta.env.PUBLIC_NARAVM_WASM_URL ?? '/naravm.wasm';
}

let modulePromise: Promise<WebAssembly.Module> | null = null;

function getWasmModule(): Promise<WebAssembly.Module> {
  if (!modulePromise) {
    modulePromise = (async () => {
      const url = naravmWasmUrl();
      // Prefer streaming compile; fall back to ArrayBuffer for servers
      // without the application/wasm MIME type.
      if (typeof WebAssembly.compileStreaming === 'function') {
        try {
          return await WebAssembly.compileStreaming(fetch(url));
        } catch {
          // Fall through to the ArrayBuffer path below.
        }
      }
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status} fetching ${url}`);
      }
      const bytes = await response.arrayBuffer();
      return await WebAssembly.compile(bytes);
    })();
    // Allow retry after a failed load (missing artifact, bad MIME, ...).
    modulePromise.catch(() => {
      modulePromise = null;
    });
  }
  return modulePromise;
}

export async function runNaravmBytecode(bytecode: Uint8Array): Promise<NaravmRunResult> {
  const module = await getWasmModule();
  let memory: WebAssembly.Memory | null = null;
  const decoder = new TextDecoder();
  const stdoutChunks: Uint8Array[] = [];
  const stderrChunks: Uint8Array[] = [];

  const copyOut = (ptr: number, len: number): Uint8Array => {
    if (!memory) throw new Error('WASM instance is not attached');
    if (len === 0) return new Uint8Array(0);
    // Copy immediately: the buffer detaches if memory grows later.
    return new Uint8Array(memory.buffer.slice(ptr, ptr + len));
  };

  const imports: WebAssembly.Imports = {
    env: {
      host_stdout_write: (ptr: number, len: number) => {
        stdoutChunks.push(copyOut(ptr, len));
      },
      host_stderr_write: (ptr: number, len: number) => {
        stderrChunks.push(copyOut(ptr, len));
      },
    },
  };

  const instance = await WebAssembly.instantiate(module, imports);
  const exports = instance.exports as Partial<WasmExports>;
  if (
    !(exports.memory instanceof WebAssembly.Memory) ||
    typeof exports.alloc_input !== 'function' ||
    typeof exports.free_input !== 'function' ||
    typeof exports.run_bytecode !== 'function' ||
    typeof exports.get_last_error_ptr !== 'function' ||
    typeof exports.get_last_error_len !== 'function'
  ) {
    throw new Error('Naravm WASM exports are missing');
  }
  memory = exports.memory;

  const concat = (chunks: Uint8Array[]): Uint8Array => {
    const total = chunks.reduce((n, c) => n + c.length, 0);
    const out = new Uint8Array(total);
    let offset = 0;
    for (const c of chunks) {
      out.set(c, offset);
      offset += c.length;
    }
    return out;
  };

  const ptr = exports.alloc_input(bytecode.length);
  if (!ptr) throw new Error('WASM input allocation failed');
  try {
    new Uint8Array(memory.buffer, ptr, bytecode.length).set(bytecode);
    const status = exports.run_bytecode(ptr, bytecode.length) >>> 0;
    const statusName = STATUS_NAMES[status] ?? `status_${status}`;
    let error = '';
    if (status !== 0) {
      const eptr = exports.get_last_error_ptr();
      const elen = exports.get_last_error_len();
      if (elen > 0) error = decoder.decode(copyOut(eptr, elen));
    }
    return {
      status,
      statusName,
      stdout: decoder.decode(concat(stdoutChunks)),
      stderr: decoder.decode(concat(stderrChunks)),
      error,
    };
  } finally {
    try {
      exports.free_input(ptr, bytecode.length);
    } catch {
      // Best effort; a failed free must not hide the run result.
    }
  }
}

/// Render a run result as playground output lines: program stdout first,
/// then stderr, then a one-line exit status.
export function formatRunLines(run: NaravmRunResult): string[] {
  const lines: string[] = [];
  if (run.stdout) lines.push(...run.stdout.replace(/\n$/, '').split('\n'));
  if (run.stderr.trim()) {
    lines.push(...run.stderr.replace(/\n$/, '').split('\n').map((l) => `stderr: ${l}`));
  }
  if (run.status === 0) {
    if (!run.stdout) lines.push('(no output)');
    lines.push('exit ok');
  } else {
    lines.push(`exit ${run.statusName}${run.error && run.error !== run.stderr.trim() ? ` — ${run.error}` : ''}`);
  }
  return lines;
}
