# Project module resolution plan

Status: implemented for monomorphic source function imports

## Goal

Make source modules discovered through a project's `vl.toml` available to
other source files through the existing `use` syntax. Given:

```toml
module = "root"
source = "src"
```

and:

```text
src/
  main.vl
  otherfile.vl
  util/math.vl
```

the compiler must register these modules before resolving any one file:

```text
root.main
root.otherfile
root.util.math
```

This must make the following forms work:

```vl
use root.otherfile;
use root.util.math.add;
use root.util.math.{sub, mul};

fun main() {
    otherfile.run();
    val a = add(1u64, 2u64);
    val b = sub(a, 1u64);
    val c = mul(b, 2u64);
}
```

There is no visibility syntax in this iteration. Top-level functions are
public exports by default. Adding `pub`, private items, package dependencies,
or re-exports is not part of this change.

The first delivery should cover functions end to end, including code
generation. Merely making E202 disappear is not sufficient: the generated
call must identify and invoke the function in the other module's artifact.
Cross-module globals and object types are called out separately below because
the current IR and Naravm module-state design cannot represent them safely.

## Language and project contract

### Canonical module names

Keep the mapping already implemented by `project_module_for_file` in
`src/main.rs`:

- The `module` value in `vl.toml` is the absolute project namespace.
- Every `.vl` file below `source` contributes its relative path without the
  extension.
- Path separators become dots.
- `src/main.vl` is `root.main`; it is not implicitly shortened to `root`.
- `src/util/math.vl` is `root.util.math`.
- Module segments continue to obey `validate_module_name`: ASCII identifier
  syntax separated by dots.
- Module names and export names are case-sensitive.

The namespace `root` does not itself imply a source file. Therefore
`use root;` is valid only if the project eventually defines an actual module
with that canonical name; this plan does not synthesize one.

### Import behavior

Preserve the current import forms and precedence:

1. `use root.otherfile;` first checks whether the full path is a module. If it
   is, it binds the leaf alias `otherfile`.
2. If the full path is not a module, the resolver treats the final segment as
   a single export from the parent module. Thus `use root.otherfile.run;`
   binds `run`.
3. `use root.otherfile.{run, stop};` requires the path before the group to be
   an exact module and binds each named export.
4. `use` is local to the importing source file. It does not re-export an item.
5. Imports are absolute. This iteration does not add `self`, `super`, relative
   imports, glob imports, aliases, or search-path fallback.
6. `std` remains a compiler-owned namespace. Project modules and target
   modules share one lookup table, and a collision is diagnosed rather than
   resolved by ordering.

The import alias must participate in normal duplicate-name checks. Two imports
that bind the same alias, or an import that conflicts with a top-level
definition, must produce one focused diagnostic instead of silently replacing
the earlier binding.

### Visibility

All top-level `fun` declarations except compiler-synthesized functions are
exports. The interface collector must not special-case underscore-prefixed
names. `main` remains subject to its existing entrypoint signature checks; it
does not acquire a separate visibility rule.

Generic functions are also public. It is acceptable to land monomorphic calls
first, but the feature is not complete for generic exports until concrete
instances requested by another module are emitted by the module that owns the
generic definition.

### Project entrypoint

A project may contain zero or one `main` function across all source modules.
Zero keeps library projects valid. More than one must be rejected before
backend emission, because Naravm accepts only one `<entrypoint>` among the set
of loaded artifacts. Do not require `main` to live in `root.main`; that file is
a convention, not a language rule.

## Current behavior and root cause

The parser already accepts every required `use` form. No lexer or grammar
change is needed for function imports.

The E202 comes from the way a project is compiled today:

1. `build_project` discovers all `.vl` files and derives correct canonical
   module names.
2. It immediately calls `compile_project_source` for each file in isolation.
3. `compile_project_source` passes only `vl_codegen::modules()` (or the
   target-specific variant) to `vl_semantic::resolve_with_modules`.
4. Those catalogs contain `std`, `std.fs`, and `std.string`, but no modules
   discovered from the project.
5. `vl-semantic` therefore correctly reports E202 for `root.otherfile` from
   the incomplete catalog it received.

There are additional blockers after resolution:

- `vl_common::ModuleSpec` models compiler-owned function exports but does not
  distinguish a source module from a target module or describe generic
  functions.
- `vl-semantic::DefKind::External` conflates target externs and imported
  source functions.
- HIR stores an `external` boolean and a display name, not a stable qualified
  callee.
- `vl_lir::Instr::Call` stores only `callee: String`, so `math.add` loses the
  canonical provider module before codegen.
- The Naravm backend treats a user-call name as local to `prog.module` and
  hard-codes the supported `std` calls. It cannot currently produce a
  `Function { module: root.otherfile, function: run }` constant from LIR.

The implementation must address all of these layers.

## Proposed compiler model

### Shared qualified identities and interfaces (`vl-common`)

Keep `ModulePath`, but make its invariants explicit and add a qualified symbol
identity. A representative shape is:

```rust
pub struct SymbolRef {
    pub module: ModulePath,
    pub name: String,
}

pub enum ModuleOrigin {
    Source,
    Target,
}

pub struct ModuleInterface {
    pub path: ModulePath,
    pub origin: ModuleOrigin,
    pub functions: Vec<FunctionExport>,
}

pub struct FunctionExport {
    pub name: String,
    pub sig: FunctionSig,
}
```

`FunctionSig` must carry the information currently split between
`vl_common::FuncSig` and `vl_typecheck::FuncSigTy`:

- parameter names and `VlType`s;
- return `VlType`;
- declared type parameter names;
- each type parameter's optional `GenericBound`.

Prefer extending/renaming the existing shared signature types rather than
creating a second almost-identical catalog just for project sources. Keep
typechecker-only `Ty` values out of `vl-common`; interfaces use surface
`VlType`s and are converted by `vl-typecheck`.

`ModulePath` should gain helpers such as `from_dotted`, `segments`, `leaf`,
and an allocation-free comparison where practical. Do not continue creating
synthetic paths whose first segment itself contains dots, as
`sig_for_bare_import` currently does.

### Parsed project units (driver)

Refactor project orchestration out of the per-file loop into a project-wide
pipeline. `src/project.rs` is preferable to making `src/main.rs` larger. The
driver remains responsible for the filesystem and manifests; library crates
must not read `vl.toml` or walk directories.

Use a structure along these lines:

```rust
struct ProjectUnit {
    path: PathBuf,
    module: ModulePath,
    text: String,
    tokens: Vec<Token>,
    ast: Program,
    diags: Vec<Diagnostic>,
}
```

The project pipeline should have explicit phases:

```text
discover/read/name every source
        ↓
lex and parse every source (with recovery)
        ↓
collect source-module interfaces
        ↓
merge source and target module catalogs
        ↓
resolve/typecheck/lower every source
        ↓
coordinate cross-module generic instances
        ↓
emit all artifacts
```

Do not resolve the first file before the last file has at least been parsed
and had its interface collected. File order must not affect whether an import
works.

Each diagnostic remains owned by one `ProjectUnit`, matching the current
single-file `Diagnostic`/`Span` design. Project-graph errors should be anchored
to the relevant `use` or declaration span and may name other files/modules in
a note. Multi-file Ariadne labels are not required for this feature.

### Interface collection (`vl-semantic`)

Add a recovery-friendly API which extracts the public surface from a parsed
program without resolving function bodies, for example:

```rust
pub fn collect_interface(program: &Program)
    -> (ModuleInterface, Vec<Diagnostic>);
```

For every top-level function, it should:

- record the canonical module from `Program.module`;
- record the source name exactly once;
- retain parameter names, parameter types, return type, generic parameters,
  and bounds;
- poison a malformed signature instead of inventing a usable export;
- diagnose duplicate export names at their declaration sites;
- continue collecting unrelated exports after an error.

Missing/invalid annotations are already parser diagnostics. Such declarations
may remain in the interface as poisoned entries so an importer does not also
receive a misleading E203. Calls to a poisoned entry must stay quiet after the
provider's root diagnostic.

The merged catalog must be indexed by canonical module string. Reject:

- duplicate canonical source modules;
- a source module colliding with a target module such as `std`;
- duplicate exports in one interface;
- an invalid configured root or derived path (existing E602 behavior can stay
  in the driver for manifest/path failures).

### Name resolution (`vl-semantic`)

Replace the current `imports: HashMap<String, ModuleSpec>` plus synthetic
singleton `ModuleSpec`s with explicit bindings:

```rust
enum ImportBinding {
    Module(ModulePath),
    Function(SymbolRef),
    Poisoned,
}
```

An imported function definition should carry:

- `DefKind::ImportedFunction` (separate from target externs);
- its `SymbolRef`;
- its shared function signature;
- the import/use span needed for diagnostics.

Target functions should similarly retain a qualified identity (`std.print` is
module `std`, symbol `print`) rather than depending on a flattened string.

Resolution then becomes:

- a local `fun` use resolves to its local `DefId` and a symbol in the current
  module;
- `otherfile.run()` resolves the alias `otherfile`, looks up `run` in the
  bound module interface, and records `root.otherfile::run`;
- a direct/grouped import resolves to the same qualified symbol while binding
  a bare local alias;
- an unknown exact module is E202;
- a known module with an unknown function is E203;
- a poisoned import prevents a later E201 cascade at each use;
- alias collisions get one new resolver diagnostic (use the next available
  E20x code and document it in the semantic tests).

Qualified expression paths deeper than `alias.export` should remain rejected
for now. The absolute path belongs in `use`; expressions refer through the
local leaf alias, as they do for `std.string` today.

Function-import cycles do not need to be rejected. Interfaces exist before
bodies are resolved, so `a` may call a public function in `b` while `b` calls
one in `a`.

### HIR and type checking

Replace the lossy HIR call fields (`external`, flattened `name`) with a
resolved callee record. It must preserve:

- local `DefId` when the callee is defined in this HIR program;
- qualified `SymbolRef` for every callee;
- callee origin (local source, imported source, or target extern);
- the shared signature for non-local callees;
- the source spelling only when useful for a diagnostic.

Local function checking should continue using the existing `func_sigs` map.
Imported source calls should reuse the same arity, type-argument, bound,
argument-coercion, and return-type rules as local calls. Do not route them
through the current restricted "externs are never generic" branch. Target
externs may keep that restriction until a target interface declares generic
parameters.

Nominal object names appearing anywhere inside an exported function signature
must eventually be canonicalized (for example,
`root.models::User`, including inside `Array[User]`). Until the object-module
work described below lands, reject a cross-module signature containing a
user-defined object with one explicit unsupported-feature diagnostic. Do not
compare unqualified `Object("User")` values from different modules as if they
were the same type.

### Cross-module generic instances

The existing monomorphizer emits an instance into the module containing the
generic HIR body. A call from another module therefore needs project-level
coordination; emitting the mangled name only in the caller would leave the
provider artifact without that function.

Implement this as a worklist owned by the project driver/compiler
orchestrator:

1. Typecheck every module and collect imported generic requests keyed by
   `(owner module, exported function, concrete type arguments)`.
2. Feed each request to the owning module's existing monomorphization pass.
3. While expanding an instance, collect any further local or imported generic
   requests it creates.
4. Iterate until the worklist is empty, deduplicating by the fully qualified
   function plus normalized concrete arguments.
5. Reuse the existing expanding-recursion checks and instance budget; include
   the module in mangled-instance bookkeeping so equal names in different
   modules cannot collide.
6. Emit the generated instance only in the owner module's artifact. The caller
   targets that module and the agreed mangled function name.

A practical staged rollout may temporarily reject imported generic calls with
a focused diagnostic after monomorphic calls work, but that restriction must
be visible in tests/docs and removed before declaring public function imports
complete.

### LIR

Make call identity structural and target-neutral. For example:

```rust
pub struct FunctionRef {
    pub module: String,
    pub function: String,
}

pub struct FunctionImport {
    pub symbol: FunctionRef,
    pub param_tys: Vec<Ty>,
    pub ret: Ty,
}
```

Then either store a `FunctionRef` directly in `Instr::Call` and keep
signatures in `LirProgram::imports`, or use a stable import-table index. The
important invariants are:

- local, imported, and target calls all retain a module and function;
- backends do not recover module identity by parsing a string;
- runtime-normalized parameter and return types are available to backends for
  the call ABI;
- LIR remains target-independent;
- LIR dumps show qualified calls, e.g.
  `call root.otherfile::run(...)`, so golden failures are understandable.

Local monomorphized calls should also use a qualified `FunctionRef`, even when
the module equals `LirProgram.module`.

### Naravm backend

Naravm already represents a function constant as a module string plus a
function string, and its CLI can register multiple vmfiles before executing
the unique entrypoint. Use that facility directly; do not modify the Naravm
checkout.

Refactor call emission so it no longer branches on flattened names:

- if `callee.module == prog.module`, find the local function signature and
  constant as today;
- if the callee is a supported target extern, use the qualified target module
  and its backend implementation;
- if the callee is an imported source function, intern both the provider
  module name and emitted function name, add a Naravm `Function` constant,
  and use the normal VL calling convention before `calli`;
- obtain argument lanes and return handling from the LIR import signature,
  rather than from `ctx.sigs`, which only contains local functions;
- deduplicate imported function constants by `FunctionRef`;
- retain the caller spill/restore behavior around a cross-module call.

The provider artifact continues to publish ordinary Naravm functions under
its own canonical module name. A runnable project supplies all generated
`.naravm` files to Naravm; only the module containing VL's sole `main` emits
`<entrypoint>`.

Add a backend test that decodes or inspects the generated constant pool and
proves a call from `root.main` targets module `root.otherfile`, function
`run`. Checking only that both strings occur somewhere in the byte array is
not strong enough if a small decoder/helper can assert the `Function`
constant relationship.

## Driver and CLI behavior

### Project build

Replace `compile_project_source` for semantic/LIR/backend builds with a
project compilation result containing one frontend result per source module.
Token and AST emits may remain per-file shallow operations.

For semantic builds:

- parse all files before resolving any file;
- use the broad target catalog for `--emit lir` and the target-specific
  catalog for final/assembly emission, exactly as today;
- merge the selected target catalog with source interfaces;
- collect and print diagnostics for every file instead of stopping at the
  first broken module;
- do not emit any final project artifact if any source has an error. This
  avoids producing a mutually inconsistent subset of a module set;
- keep output naming unchanged:
  `root.otherfile -> out/root__otherfile.naravm`;
- detect output-path collisions before writing any artifact;
- write artifacts in deterministic canonical-module order.

Token/AST output can still be written for malformed files because it does not
claim that a linked project is usable.

### `vl check`

Module resolution should not work only under `vl build`. Make checking
project-aware:

- when the checked file is inside the `source` tree of a discoverable
  `vl.toml`, load and check the project catalog so its imports resolve;
- when no project contains the file, retain standalone checking with only the
  compiler-owned module catalog;
- factor project discovery and compilation so `check` and `build` cannot
  drift;
- document whether project-aware `check path/to/file.vl` reports diagnostics
  for the full project or filters successful unrelated units. The recommended
  behavior is to report all project errors because provider interfaces may be
  invalid even when the requested file parses cleanly.

Do not make single-file `build path/to/file.vl` silently depend on stale
artifacts. Either promote it to a full project build when the file belongs to a
project, or keep it explicitly standalone and diagnose project imports with a
note to run `vl build`. The recommended behavior is promotion to the full
project build, because a cross-module Naravm call is not runnable without the
provider artifacts.

## Diagnostics and recovery

Keep all errors as `vl_common::Diagnostic` and render them only in the driver.
No library crate should print.

Required cases:

| Situation | Result |
|---|---|
| `use root.missing;` and no such module | one E202 at the `use` |
| module exists, export does not | one E203 at the `use` |
| use of an alias poisoned by E202/E203 | no follow-on E201/E303 |
| duplicate import alias | one resolver error with both local spans when they are in the same file |
| import conflicts with local top-level name | one resolver error, deterministic winner, uses stay poisoned |
| provider signature is malformed | provider reports the root parse/type error; importers do not add E203 |
| source module collides with `std` | one project/catalog diagnostic before resolution |
| multiple project `main`s | one project-level entrypoint diagnostic per extra declaration, no backend emission |
| imported call has wrong arity/type/bound | the normal E303/E306-style call diagnostic at the caller |
| backend lacks a target extern | existing target-specific diagnostic behavior |

Continue resolving independent imports and bodies after an error. A broken
module must not prevent valid sibling modules from being checked, although any
project error blocks final artifact emission.

## Explicitly deferred item categories

"Public by default" is straightforward for function declarations because
their complete boundary types are present in the syntax and Naravm already has
cross-module function references. The following declarations require separate
design work and should not be accidentally accepted by treating them as
functions.

### Object types

Cross-module object types need:

- paths in type and object-literal syntax, or a type-aware import namespace;
- fully qualified nominal identities in `VlType`, HIR, typed HIR, and LIR;
- imported layout metadata for field access/allocation in the caller backend;
- collision rules between value, function, and type namespaces.

Until that work lands, source interfaces should detect object types inside
cross-module function signatures and emit a clear unsupported-boundary
diagnostic rather than mis-typing two modules' `User` objects as the same
type.

### Top-level `val`/`var`

Imported globals need more than a type in the module catalog:

- inferred globals need an interface-elaboration order or an explicit type
  requirement;
- assignments need an imported-value resolution path and mutability policy;
- LIR needs qualified global load/store operations;
- each Naravm module needs persistent, independently addressable module state;
- library initialization order and cycles must be specified.

The current Naravm backend keeps one module-state container in reserved
register `rf3F`. That is insufficient for simultaneously live state belonging
to multiple artifacts, and the Naravm checkout must not be modified from this
repository. Defer imported globals until VL has a codegen strategy for a
per-module state handle (or a VL-side linker). Local globals continue to work.

If the phrase "everything public by default" is intended to require object
types and globals in this same delivery, settle these two designs before
implementation; they are not a small extension of E202 resolution.

## Implementation sequence

Each step should leave the workspace compiling and should be committed by
pipeline stage where practical.

1. **Define the contract and shared identities (`vl-common`).**
   Add structural module/symbol/function-signature types, generic metadata,
   constructors, and unit tests. Migrate target module declarations without
   changing behavior.
2. **Collect source interfaces (`vl-semantic`).**
   Extract public function declarations from parsed programs, preserve poison,
   and test duplicates/malformed signatures.
3. **Resolve source imports (`vl-semantic`).**
   Replace synthetic module specs with explicit module/function bindings;
   distinguish imported source functions from target externs; add collision,
   direct, grouped, qualified, cycle, E202/E203, and poison tests.
4. **Preserve qualified callees (`vl-hir`).**
   Carry `SymbolRef` and origin through lowering. Add focused lowering tests
   for local, source-imported, and `std` calls.
5. **Type imported calls (`vl-typecheck`).**
   Reuse local function-call rules against shared signatures. Cover arity,
   argument and return types, capability types, explicit/inferred generic
   arguments, bounds, and poison suppression.
6. **Make calls structural (`vl-lir`).**
   Add qualified call targets/import signatures, update dumps and runtime
   validation, and migrate every backend/test from string callees.
7. **Emit cross-module Naravm calls (`vl-codegen`).**
   Build imported function constants, use imported ABI signatures, preserve
   register state, and test the encoded provider/function pair. Keep all
   target-specific logic here.
8. **Build a project-wide driver pipeline.**
   Discover/read/parse all units, collect and merge interfaces, run all
   frontends, enforce one entrypoint, and emit only after the whole project is
   clean. Reuse it from project-aware `check` and `build`.
9. **Coordinate generic instances.**
   Add the cross-module instance worklist and emit requested instances in
   their owner artifacts. Test duplicate requests, transitive requests,
   mutually recursive modules, and the existing expansion budget.
10. **Update user documentation.**
    Update the README project section and website module guide with canonical
    project paths, all three import forms, public-by-default functions,
    project-aware command behavior, how to pass all artifacts to Naravm, and
    the explicit object/global limitation.
11. **Run the full gate.**
    Run `./scripts/check.sh`, then manually build a multi-file project and, if
    the local Naravm binary is available, execute all its artifacts together.

Suggested commit boundaries:

```text
refactor(vl-common): model qualified module symbols
feat(vl-semantic): collect project module interfaces
feat(vl-semantic): resolve project function imports
feat(vl-hir): preserve qualified callees
feat(vl-typecheck): check imported source calls
feat(vl-lir): encode qualified calls
feat(vl-codegen): emit Naravm cross-module calls
feat(vl): compile project modules as one unit
feat(vl-typecheck): coordinate imported generic instances
docs: document project module imports
```

## Test matrix

### Unit tests

- `vl-common`: dotted-path construction, equality/hashing, `SymbolRef`, generic
  interface signatures.
- `vl-syntax`: retain regression tests showing that module, direct, and
  grouped imports parse; no grammar expansion is expected.
- `vl-semantic`: exact-module precedence, each import form, missing module,
  missing export, duplicate aliases, local/import collision, source/target
  collision, poisoned imports, forward exports, and cyclic function imports.
- `vl-hir`: qualified identities survive lowering without flattening.
- `vl-typecheck`: scalar/reference arguments, return values, wrong arity,
  wrong types, generic inference/explicit arguments/bounds, and imported
  poison.
- `vl-lir`: dump format includes module and function; imported signature is
  runtime-normalized.
- `vl-codegen`: Naravm constant refers to the provider module and function;
  call ABI handles value, reference, and void-return cases.

### Project integration tests (`tests/project.rs`)

Create temporary projects for at least:

1. `root.main -> root.otherfile::run` using a module-qualified call.
2. Direct and grouped imports from a nested module.
3. Provider file discovered after importer in lexical sort order, proving
   order independence.
4. An imported return value used in a typed expression.
5. A cross-module generic request emitted in the provider artifact.
6. Mutual function imports with no globals.
7. E202 for a truly missing project module, with no cascading E201.
8. E203 for a missing export in an existing project module.
9. Duplicate import/local aliases.
10. Two `main` functions rejected before artifacts are written.
11. A broken provider plus a separate broken consumer both reporting their
    root diagnostics in one invocation.
12. `vl check` resolving the same project import that `vl build` resolves.
13. Output names remaining `root__path__module.<extension>`.
14. No final outputs written when any project unit has an error.

### End-to-end smoke project

Use a checked-in fixture or construct a temporary project equivalent to:

```toml
module = "demo"
```

```vl
// src/lib/math.vl
fun twice(value: u64): u64 {
    return value + value;
}
```

```vl
// src/main.vl
use demo.lib.math.twice;
use std;

fun main() {
    std.print_u64(twice(21u64));
}
```

Acceptance requires:

- `vl check src/main.vl` succeeds in project context;
- `vl build` produces artifacts for `demo.main` and `demo.lib.math`;
- the `demo.main` artifact contains a function reference to
  `demo.lib.math::twice`;
- loading both artifacts into Naravm prints `42`;
- removing `math.vl` changes the result to exactly one E202 at the import.

## Definition of done

The function-module feature is complete when:

- project source modules are discovered once and resolved independently of
  file order;
- all current `use` forms work against public source functions;
- qualified provider identity survives AST resolution through backend output;
- ordinary and generic imported calls receive the same type checks as local
  calls;
- Naravm artifacts call the provider module rather than assuming the current
  module;
- checking and building agree on module resolution;
- errors recover without cascades and all final diagnostics use Ariadne;
- project builds do not leave a partially updated artifact set after a
  compiler error;
- documentation clearly states the canonical path rules and the temporary
  object/global boundary;
- `./scripts/check.sh` passes.
