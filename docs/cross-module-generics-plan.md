# Cross-module generics and generic stdlib helpers

## Status

Implementation plan. This document describes compiler work; it does not change
the language yet.

## Summary

VL already supports generic functions within one source module. A generic body
is checked once using opaque `Ty::Param` values, and concrete call sites create
monomorphized functions such as `first$u64`. That implementation stops at a
module boundary.

The current cross-module restriction is deliberate and enforced in several
places:

| Area | Current behavior |
|---|---|
| `vl-common` | `FuncSig` has parameters and a return type, but no type parameters or bounds. `ModuleSpec` stores generic exports only as names in `generic_exports`. |
| `vl-semantic` | `collect_interface` omits generic signatures from `exports`. Importing or calling one reports `E207`. |
| `vl-hir` | Imported calls carry a monomorphic `extern_sig`; there is no body/template identity beyond `SymbolRef`. |
| `vl-typecheck` | Imported calls take an extern-only path which rejects all explicit type arguments. The monomorphizer can only find templates and `DefId`s in its current `HirProgram`. |
| `vl-lir` | Instances are lowered only from the current module. An imported `symbol` overrides the local mangled call mapping. Import collection scans raw generic templates rather than only emitted concrete code. |
| Driver | Project modules are checked and emitted one at a time after interface collection. There is no project-wide specialization fixed point. |
| `vl-stdlib` | `load` asserts that every helper is monomorphic and that no lowered helper name contains `$`. It stores only already-lowered monomorphic bodies. |

The end state should preserve VL's ahead-of-time model: generics do not exist in
LIR, backend input, artifacts, or the VM. The project compiler discovers every
concrete instantiation, emits each source instance in the module that owns its
template, and makes callers import that concrete symbol. Stdlib instances use
the same discovery process but are copied into the consuming artifact because
embedded stdlib helpers do not have separate runtime artifacts.

## Goals

- Allow every existing import form to name a generic source function:
  `use app.lib;`, `use app.lib.id;`, and `use app.lib.{id}`.
- Support inferred and explicit type arguments across modules.
- Preserve bounds, capabilities, qualified object identity, generic forwarding,
  recursive generics, deduplication, and the instantiation budget.
- Allow embedded VL stdlib helpers to be generic.
- Emit only concrete functions and concrete function imports in LIR.
- Keep errors in their owning source module and preserve poison-without-cascade
  behavior.
- Keep output deterministic and project publication transactional.

## Non-goals

- Runtime dictionaries, erased generics, VM changes, or backend generic support.
- Generic object/type declarations; this plan covers generic functions only.
- Generic VM-native exports. Target exports remain monomorphic; the existing
  compiler-special `Array.new` path is unchanged.
- Supporting imported functions that depend on module globals. The existing
  `E208` restriction remains independent of generic support.
- Persisting templates for separately distributed packages. The first version
  supports source modules in the current project and compiler-embedded stdlib
  modules, both of which have bodies available during compilation.
- Changing generic syntax. The lexer, parser, and grammar need no syntax work.

## Design decisions

### Specializations belong to the template owner

For a call from `app.main` to `app.lib::id[u64]`, emit
`id$<u64-encoding>` in the `app.lib` artifact and emit a concrete import from
`app.main` to that symbol. If several modules request the same instance, the
owner emits it once.

This avoids cloning arbitrary project code into callers, keeps local calls and
object identities in their defining module, and matches the existing project
artifact model. It also makes mutually recursive module graphs a fixed-point
discovery problem rather than an inlining problem.

Stdlib is the one linkage exception, not a second generic implementation. A
requested stdlib instance is first generated as an owner-module function using
the same typecheck/monomorphization machinery. `vl-stdlib` then copies the
referenced concrete body into the consumer, as it already does for monomorphic
helpers.

### Interfaces carry complete generic signatures

A generic function must be a normal export with complete callable metadata,
not a name in a deny-list. Add a shared type-parameter signature:

```rust
pub struct TypeParamSig {
    pub name: String,
    pub bound: Option<GenericBound>,
}

pub struct FuncSig {
    pub type_params: Vec<TypeParamSig>,
    pub params: Vec<ParamSig>,
    pub ret: VlType,
}
```

Monomorphic compiler/target exports use an empty `type_params`. Remove
`ModuleSpec::generic_exports` and `ModuleInterface::generic_functions` once all
consumers use the complete signature.

The catalog must also identify whether an export has a source body or is a
target-native callable. This should be per export, not per module, because a
path such as `std.math` merges VM natives and embedded VL helpers. Add an
`ExportKind` such as `Source` and `Target`, or equivalent linkage metadata, to
`Export`.

### Instance identity is module-qualified and structural

Introduce a canonical key used throughout type checking, LIR lowering, stdlib
linking, caching, diagnostics, and tests:

```rust
pub struct TemplateKey {
    pub module: String,
    pub function: String,
}

pub struct InstanceKey {
    pub template: TemplateKey,
    pub args: Vec<Ty>,
}
```

Do not use a pre-mangled string as semantic identity. Derive the emitted symbol
from `InstanceKey` only at the LIR boundary. Replace the current underscore
concatenation with a collision-free, deterministic encoding before it becomes
a cross-artifact ABI. The encoding must include fully qualified object names
and must distinguish arrays and capability-qualified arguments. `$` can remain
the unlexable separator.

### Monomorphization runs over a checked module world

Refactor the local `mono::expand` worklist into a module-aware operation over
immutable checked modules. A checked module provides its `HirProgram`,
`TypedProgram`, local template definitions, and module path. The operation
returns a separate plan rather than mutating cached stdlib state:

```text
MonomorphizationPlan
  requested/emitted InstanceKey set
  instances grouped by owner module
  root call target by (caller module, HirId)
  nested call target by (outer InstanceKey, HirId)
  diagnostics grouped by owner module
```

The plan is shared by `check` and `build`. This keeps an embedded checked
stdlib reusable through `OnceLock`, while each compilation receives its own
instance set and budget accounting.

Keep bodies out of `vl-common::ModuleSpec`: the catalog remains a
signature/linkage interface, not serialized HIR. The driver combines checked
module data through APIs owned by `vl-typecheck`; `vl-typecheck` must not depend
on the later `vl-stdlib` crate.

## End-to-end implementation

### 1. Publish generic interfaces in `vl-common` and `vl-semantic`

1. Add type parameters and bounds to `FuncSig` and update `FuncSig::new` so
   target declarations remain easy to create with no type parameters.
2. Add per-export linkage/origin metadata so merged stdlib modules can contain
   both target natives and source helpers.
3. Change `collect_interface_impl` to publish a well-formed generic function in
   `functions` with its complete signature.
4. Continue qualifying provider-local object types in parameter and return
   positions. Preserve `VlType::Param` unchanged while recursively qualifying
   `Array`, mutable views, and object references.
5. Keep malformed generic headers in `poisoned_exports`; never publish a partial
   generic signature.
6. Remove the generic `E207` branches from all three import forms and from
   qualified lookup. Resolve generic imports to ordinary `ImportedFunction`
   definitions carrying `FuncSig`, `SymbolRef`, and source linkage.
7. Apply the existing global-dependence check to generic exports exactly as it
   applies to monomorphic exports. If an export is both generic and global
   dependent, importing it reports the single `E208` root cause.

No lexer or parser change is required. Add a short semantic note to the syntax
documentation only if it currently claims generics are module-local.

### 2. Preserve imported template identity in HIR

1. Keep the imported `SymbolRef` on `HirExpr::Call`; it is the stable
   cross-module template identity.
2. Replace the extern-only meaning of `extern_sig` with a callable signature
   that may be generic, or rename the field to avoid encoding the obsolete
   assumption.
3. Preserve source-versus-target linkage on imported calls. Do not infer it
   from whether a signature is generic.
4. Keep unresolved and poisoned imports represented with `def: None` or a
   missing signature so downstream stages remain quiet.

### 3. Unify local and imported generic call checking

Refactor the call checker so local and imported source calls use the same
signature algorithm:

1. Convert a shared `FuncSig` into `FuncSigTy`, including type parameters and
   bounds.
2. Resolve explicit type arguments or infer omitted arguments from actual
   values using the existing constraint solver.
3. Enforce generic arity, value arity, bounds, `void`, capability validity,
   contextual empty arrays, and coercions through one path.
4. Continue treating target-native exports as monomorphic; explicit type
   arguments on them still report `E303`.
5. For a concrete imported source call outside a generic body, record an
   `InstanceKey` request and map the call to the concrete owner-module target.
6. For an imported generic call inside a generic body, record enough generic
   call facts to resolve it after substituting the outer instance. The current
   `calls_in_item` traversal already records the needed call ID, explicit type
   arguments, and actual argument types; generalize its callee identity from a
   local `DefId` to `TemplateKey`.
7. Keep definition-site checking unchanged in principle: every generic body is
   checked once with opaque parameters even if it is never instantiated.

Imported-call diagnostics belong to the caller because inference and argument
checking happen there. Invalid provider bodies continue to be diagnosed while
checking the provider, before monomorphization starts.

### 4. Build a project-wide monomorphization fixed point

Create a module-aware worklist in `vl-typecheck`:

1. Index every source template by `TemplateKey`. The index includes project
   modules and checked embedded stdlib modules.
2. Seed the queue with concrete requests discovered in monomorphic functions
   and global initializers across all project modules, plus monomorphic stdlib
   helper bodies that may later be linked. Planning those small intrinsic
   stdlib dependencies eagerly is acceptable; final body copying remains lazy.
3. Pop a canonical `InstanceKey`, skip it if already visited, and find the
   checked template in its owner module.
4. Instantiate its signature and record the instance under the owner module.
5. Walk calls in the template body under the instance substitution environment.
6. Resolve local or imported generic callees to module-qualified instance keys,
   map each nested call to its concrete target, and enqueue unseen instances.
7. Leave monomorphic local calls owned by the template module and preserve
   imported monomorphic `SymbolRef`s.
8. Treat same-type recursion as a cache hit. Count structurally new instance
   keys against a deterministic build budget and report expanding polymorphic
   recursion as `E303` rather than hanging.
9. Process roots and newly discovered keys in sorted order so LIR and
   diagnostics do not depend on filesystem or hash-map iteration order.

The existing limit of 64 can remain initially, but define whether it is per
project, per template, or per root. Prefer a per-compilation total for the first
implementation because it is simple and bounded; include the count and owner
template in the diagnostic. Route worklist diagnostics to the source unit that
owns the template span.

### 5. Refactor the driver into batch frontend phases

`build_project_at` and `check_project` currently invoke the complete frontend
one unit at a time. Replace that flow with explicit project phases:

1. Discover, read, lex, and parse every source unit.
2. Collect every module interface and form one catalog with target and stdlib
   interfaces.
3. Resolve, lower to HIR, and typecheck every clean source unit, retaining its
   AST/HIR/typed result instead of immediately lowering to LIR.
4. If any frontend errors exist, emit them by source unit and stop before
   monomorphization or artifact generation.
5. Combine checked project modules with immutable checked stdlib modules and
   run the global monomorphization fixed point.
6. Validate normalized types for ordinary code and every planned instance. A
   surviving `Int`, `Param`, or nested `Error` remains an `E500` boundary bug.
7. For `check`, stop successfully after this validation.
8. For `build`, lower all project modules using the complete instance plan,
   link requested stdlib bodies, then invoke the backend for every module.
9. Preserve the existing all-or-nothing staging and publication behavior.

Do not emit a provider artifact before all importers have been checked: a later
module may request another specialization that belongs in that provider.
Module cycles are allowed because interface collection precedes checking and
the specialization queue is a fixed point.

The single-file path uses the same machinery with one project module plus
stdlib. It still cannot import arbitrary source modules without a project
catalog.

### 6. Lower concrete project instances in their owner modules

Update `vl-lir` to consume the module-aware plan:

1. Emit non-generic functions as today.
2. For each instance assigned to the current owner module, find its local HIR
   template, apply its type substitution, and emit one concrete `Function`.
3. Resolve calls through the plan before falling back to the HIR symbol. This
   fixes the current ordering where an imported `symbol` would override a
   mangled generic call target.
4. Emit a local call when the concrete target belongs to the current module and
   a `FunctionImport` when it belongs to another module.
5. Build imports from emitted concrete code, or make the import collector aware
   of instance substitutions and call mappings. Do not scan uninstantiated
   generic templates and do not import the unmangled generic source name.
6. Deduplicate imports by concrete symbol and verify that repeated uses agree
   on their concrete parameter and return types.
7. Keep capability erasure at the existing LIR boundary.
8. Sort owner instances and imports by canonical key for deterministic dumps and
   artifacts.

Backends should require no generic-specific branch. They continue to see local
concrete functions and concrete cross-module imports only. Add a defensive LIR
validation failure if any generic parameter or unresolved template name reaches
code generation.

### 7. Make embedded stdlib loading template-aware

Refactor `vl_stdlib::load` from independent one-module compilation into a
two-pass embedded project build:

1. Lex and parse all embedded modules.
2. Collect all helper interfaces before resolving any helper body, then merge
   them with target-native exports. This also makes cross-stdlib helper calls
   independent of source order.
3. Resolve, lower, and typecheck every helper module against the merged catalog.
4. Keep the authoring rules forbidding `main`, globals, collisions with native
   exports, and imports outside stdlib or Naravm-emittable natives.
5. Remove the assertions that helper interfaces and lowered names are
   monomorphic.
6. Store immutable checked modules as the source of truth. Existing pre-lowered
   monomorphic bodies may be retained only if they contain no plan-sensitive
   generic calls; otherwise lower them per compilation with the instance plan.
7. Expose the checked templates to the per-compilation monomorphization world.
8. After specialization, lower monomorphic helpers and requested concrete
   instances into a per-compilation stdlib link set. This ensures a
   monomorphic helper calling a generic helper also uses its concrete mapped
   symbol.
9. Extend `Stdlib::link` to recognize concrete generic symbols, copy only bodies
   reachable from the consumer, rewrite them to collision-proof local names,
   and carry required native imports as it does today.
10. Never mutate the process-wide `OnceLock<Stdlib>` with one project's
    instances; instance caches and budgets are compilation-local.

As the first real stdlib use, add a generic bounded helper such as
`std.math.max[T extends Numeric]` without removing the existing typed helpers.
This exercises inference, explicit arguments, bounds, generated arithmetic,
owner identity, and lazy stdlib linking without making the rollout depend on an
API migration.

### 8. Diagnostics and recovery

- Remove `E207` only for the former unsupported-feature case. Do not reuse it
  for type errors.
- Keep call-site inference, type-argument, bound, and argument diagnostics at
  the importing call span.
- Keep invalid generic-body diagnostics at the provider definition span.
- Associate fixed-point diagnostics with an owner module before returning them
  to the driver; bare `Span` offsets are not sufficient to choose a project
  source file.
- If a provider is parse/signature poisoned, preserve its name in
  `poisoned_exports` and suppress missing-export/typecheck cascades in importers.
- If monomorphization has any error, do not lower or emit any artifact.
- Do not add printing to libraries. Return `vl_common::Diagnostic` values and
  let the driver call `emit_all`.

## Test plan

### `vl-common`

- Construct monomorphic and generic signatures with bounds.
- Distinguish source and target exports after same-path catalog merging.

### `vl-semantic`

- Collect a complete generic export signature with qualified object types.
- Resolve generic calls through module, single-symbol, and grouped imports.
- Keep malformed generic exports poisoned without importer cascades.
- Keep global-dependent generic imports at one `E208`.
- Replace the current test expecting `E207` with successful resolution tests.

### `vl-hir`

- Preserve generic imported signature, source linkage, and `SymbolRef`.
- Preserve poison for an unresolved generic provider.

### `vl-typecheck`

- Verify canonical instance keys and collision-free mangling for nested arrays,
  mutable views, and fully qualified object types.
- Infer an imported `id[T]` and accept `id::[u64]`.
- Diagnose wrong type-argument count, failed inference, bound failure, wrong
  value arity, coercion failure, and invalid capabilities at the caller.
- Forward an outer `T` to a generic in another module and instantiate it after
  the outer call becomes concrete.
- Deduplicate the same instance requested by several callers.
- Cover local-to-foreign, foreign-to-local, and foreign-to-foreign generic
  calls.
- Terminate same-type recursive cycles and diagnose type-expanding recursion at
  the budget.
- Instantiate arrays, mutable views, Strings, and nominal objects from two
  modules with the same short name.

### `vl-lir`

- Emit an imported generic call to the provider's mangled concrete symbol.
- Emit the provider instance exactly once and omit the template.
- Ensure inner calls in an owner instance resolve to the correct module.
- Ensure imports contain concrete signatures and never contain `Ty::Param` or
  the original generic symbol.
- Keep dumps deterministic under reversed source-file order.

### `vl-stdlib`

- Load a generic helper without assertions.
- Infer and explicitly select its type arguments from user code.
- Link only the requested concrete instance.
- Reuse one linked instance across repeated calls and omit unused instances.
- Carry native imports used by a generic helper.
- Handle a generic helper calling another generic helper transitively.

### Integration and project tests

- Replace `imported_generic_is_rejected_with_a_focused_diagnostic` in
  `tests/project.rs` with successful `check` and `build` cases.
- Build a two-module project where the importer calls `id` for two types and
  assert both instances are in the provider artifact/LIR.
- Build a three-module generic forwarding chain.
- Build a cyclic generic call graph that converges and one that exceeds the
  budget.
- Compile and emit Naravm artifacts for a cross-module generic over a qualified
  object type.
- Compile and run an example using the new generic stdlib helper.
- Verify a failed project leaves the previous output directory untouched.
- Run `./scripts/check.sh` as the final gate.

## Suggested implementation sequence

Keep intermediate commits buildable and avoid temporarily accepting a generic
import that can reach LIR unresolved.

1. Extend shared signatures and publish generic interfaces while leaving
   semantic `E207` in place.
2. Add module-qualified instance keys, stable mangling, and checked-world data
   structures.
3. Refactor local monomorphization to use the new world API; prove existing
   single-module generic tests are unchanged.
4. Unify imported/local generic call checking and remove `E207`, but gate the
   driver on successful world monomorphization.
5. Batch project checking and emit owner-side instances plus concrete imports.
6. Refactor stdlib loading, add per-compilation stdlib specialization/linking,
   and add the first generic helper.
7. Update integration tests, examples, README project limitations, crate-level
   comments, and any grammar semantic notes.
8. Run the full gate and inspect LIR/artifact determinism with source order
   reversed.

## Acceptance criteria

The feature is complete when all of the following are true:

- `use demo.lib.id; fun main() { val x = id(1u64); }` checks and builds when
  `demo.lib` defines `fun id[T](value: T): T`.
- Inference and `::[...]` behave the same for local, project-imported, and
  stdlib generic functions.
- A concrete instance is emitted once in its project provider regardless of
  how many modules request it.
- A concrete stdlib instance is linked only into artifacts that reach it.
- Generic-to-generic calls across any mix of project and stdlib modules reach a
  deterministic fixed point.
- No `Ty::Param`, generic template, or unmangled generic import reaches LIR
  validation or a backend.
- Existing monomorphic module and stdlib behavior remains byte-for-byte stable
  when no generic helper is referenced, aside from an intentionally changed
  stable mangling format in tests that actually instantiate generics.
- Errors remain Ariadne diagnostics with one root cause and no partial project
  artifacts.
