# Read-only-by-default reference capabilities

Status: design and implementation plan; no compiler behavior has changed yet.

This plan makes mutation authority explicit without adding ownership or a
borrow checker. VL keeps one binding keyword, `let`. Local `let` bindings are
rebindable, function parameters are never rebindable, and the type spelling
controls whether a GC reference is read-only or mutable:

```vl
Foo   // read-only view of a GC-managed Foo
*Foo  // mutable view of the same kind of GC-managed Foo
```

The change applies consistently to user objects, arrays, built-in reference
types, function signatures, object fields, generics, extern signatures, HIR,
type checking, LIR lowering, every backend, examples, tests, and the website.

## 1. Language contract

### 1.1 Bindings and parameters

`let` is the only binding declaration keyword. There is no `var`, `val`, or
`const`.

```vl
let current: Foo = first;
current = second; // allowed: local lets are rebindable
```

Rebinding changes which value the local name contains. It does not mutate the
old or new heap object. A binding's inferred or annotated type is fixed after
its declaration, so every later assignment must be coercible to that type.

Function parameters use the existing `name: type` syntax and are always fixed
bindings:

```vl
fun inspect(item: Foo) {
    item = other; // error: parameters cannot be rebound
}

fun update(item: *Foo) {
    item.value = 1; // allowed
    item = other;   // error: the parameter binding is still fixed
}
```

Parameter non-rebindability is independent of reference capability. It applies
to primitive and reference parameters alike.

### 1.2 Value types and reference types

VL's value-semantic types remain:

- `u64`, `i64`, `f64`, `u8`, and `bool`;
- `void`, which is not a value and is valid only as a function return type;
- an unresolved integer literal type and `Ty::Error`, which remain internal to
  type checking.

VL's GC-managed reference types are:

- `String`;
- `File`;
- a user-defined object type such as `Foo`;
- `Array[T]` for any valid element type `T`.

An unstarred reference type is a read-only view. Prefix `*` grants mutation
authority through that view:

```vl
let view: Foo = ...;
let editable: *Foo = ...;
```

`*` is a capability qualifier, not a machine pointer. It does not introduce
address-taking, dereferencing, pointer arithmetic, null, ownership, moves,
lifetimes, borrow scopes, or unsafe memory access. VL adds no `&` operator and
no dereference expression. Passing or assigning either spelling copies the GC
reference.

The compiler rejects `*` on non-reference types:

```vl
let bad: *u64 = 1;   // error
let bad: *bool = true; // error
```

It also rejects `*void`, repeated capability qualifiers such as `**Foo`, and
`*T` where `T` is an unconstrained type parameter. A generic can still accept
or return a mutable reference by using `T` itself and inferring or explicitly
supplying `*Foo`. Known reference constructors remain usable inside mutable
types, for example `*Array[T]`.

This restriction avoids silently assuming that an unconstrained `T` is a
reference. A future `Reference` kind/bound could make `*T` well-formed without
changing the rest of this design.

### 1.3 Read-only means a view, not a frozen object

Mutation capability belongs to each reference view. It is not a permanent bit
on the heap allocation:

```vl
let editable: *Foo = Foo { value = 0 };
let view: Foo = editable;

editable.value = 2;
print(view.value); // observes 2
```

There is no exclusive mutable reference rule. Multiple `*Foo` aliases may
exist, and read-only aliases may coexist with them. A read-only alias prevents
mutation through that alias; it does not promise that the object will remain
unchanged.

Documentation and diagnostics should therefore say **read-only view** rather
than **immutable object**. This model provides const-correct APIs and prevents
accidental writes without implying freezing, snapshotting, or Rust-style
alias control.

### 1.4 Capability coercion

There is one implicit capability conversion:

```text
*Foo -> Foo    allowed: discard mutation authority
Foo  -> *Foo   forbidden: cannot invent mutation authority
```

The conversion is representation-preserving and emits no runtime operation.
It is valid at initializer, assignment, argument, return, object-field, and
array-element boundaries. The reverse direction must fail at every one of
those boundaries; routing a value through a function must never launder a
read-only view into a mutable one.

Examples:

```vl
fun read(foo: Foo) {}
fun change(foo: *Foo) {}

let editable: *Foo = Foo {};
let view: Foo = editable;

read(editable); // allowed: implicit downgrade
read(view);     // allowed
change(editable); // allowed
change(view);     // error
```

An explicit numeric `as` cast does not bypass capability checking. Reference
capability upgrades are never casts.

### 1.5 Function signatures and returns

Parameters and results state the maximum capability that crosses the function
boundary:

```vl
fun inspect(foo: Foo): Foo {
    return foo;
}

fun edit(foo: *Foo): *Foo {
    foo.value = 1;
    return foo;
}

fun create_foo(): *Foo {
    return Foo {};
}
```

A function returning `*Foo` may initialize either a mutable or read-only
binding. A function returning `Foo` cannot initialize `*Foo`:

```vl
let editable = create_foo();       // inferred *Foo
let view: Foo = create_foo();      // allowed downgrade
let bad: *Foo = inspect(view);     // error
```

This deliberately differs from returning a struct by value in Zig. VL object
results are aliases to GC allocations, so their capability must remain in the
function signature.

### 1.6 Fresh allocation expressions and inference

Fresh object and array allocations have no pre-existing aliases, so their
initial capability may be selected by an expected type. The capability is
contextual for:

- `Foo { ... }`;
- `[a, b, ...]` and an annotated empty `[]`;
- `Array.new(...)` and `Array.new::[T](...)`.

With no expected type, a fresh allocation defaults to a read-only view. This
preserves the read-only-first policy:

```vl
let view = Foo {};                 // Foo
let editable: *Foo = Foo {};       // *Foo
let values = [1, 2, 3];            // Array[u64]
let buffer: *Array[u64] = [1, 2];  // *Array[u64]
```

Expected mutable contexts also include mutable parameters, mutable returns,
mutable object fields, and mutable array element types:

```vl
fun consume(foo: *Foo) {}
consume(Foo {}); // fresh literal is *Foo in this context

fun create(): *Foo {
    return Foo {}; // return context selects *Foo
}
```

This contextual choice applies only to compiler-known fresh allocations. An
existing expression keeps its capability:

```vl
fun get_view(): Foo;
let bad: *Foo = get_view(); // error, never upgraded by context
```

String literals remain `String`. The current language exposes no mutable
string operation, so it does not manufacture `*String` from a literal merely
because a mutable context asks for one. `File` capability comes from the
extern function signature that creates or returns the handle.

### 1.7 Objects, fields, and transitive read-only projection

Field declarations carry the maximum capability stored in the field:

```vl
type Child = object {
    value: u64,
};

type Parent = object {
    child: *Child,
    children: *Array[*Child],
};
```

Reading a field combines the receiver capability with the declared field
capability. A read-only receiver downgrades a mutable reference field; a
mutable receiver preserves it:

```text
Parent.child   -> Child
*Parent.child  -> *Child
```

Apply the same rule at each later projection. This makes read-only views
transitive without changing the stored field type:

```vl
fun inspect(parent: Parent) {
    parent.child.value = 1;       // error
    parent.children[0].value = 1; // error
}

fun edit(parent: *Parent) {
    parent.child.value = 1;       // allowed
    parent.children[0].value = 1; // allowed
}
```

Field assignment itself requires a `*Object` receiver. The right-hand side
must be coercible to the field's declared type. A mutable receiver does not
upgrade a field declared with a read-only type.

### 1.8 Arrays

`Array[T]` is a read-only view of a fixed-length heap array. `*Array[T]` is a
mutable view:

```vl
fun sum(values: Array[u64], count: u64): u64 {
    // Reading is allowed.
    return values[0];
}

fun fill(values: *Array[u64]) {
    values[0] = 1; // allowed
}
```

Element assignment requires a `*Array[T]`. Indexing for reading works through
either capability. If `T` is itself a mutable reference type, indexing applies
the same projection rule as object fields:

```text
Array[*Foo][i]   -> Foo
*Array[*Foo][i]  -> *Foo
```

The element value stored in the array is unchanged; only the capability
available through the current access path is narrowed.

### 1.9 Generics

Reference capability is part of a type argument:

```vl
fun identity[T](value: T): T {
    return value;
}

let editable: *Foo = Foo {};
let same = identity(editable); // T = *Foo, result is *Foo
```

Required generic behavior:

- substitution descends through mutable reference and array types;
- mangling distinguishes `Foo` from `*Foo` even though their runtime
  representation is identical;
- explicit type arguments accept reference capabilities, such as
  `identity::[*Foo](editable)`;
- `*Array[T]` is valid because `Array[...]` is known to be a reference type;
- bare `*T` is rejected until VL has a reference-kind bound;
- generic inference preserves an actual argument's capability when solving an
  unconstrained `T`;
- if repeated constraints see both `*Foo` and `Foo`, their safe common type is
  `Foo`; inference must never select `*Foo` from a read-only actual;
- capability-insensitive readable operations may inspect the read-only base
  type, but mutation always requires an explicit mutable view.

`Numeric` continues to accept only numeric value types. `Comparable` continues
to follow the existing set of comparable base types. A mutable view may be
read as its read-only base for a comparison, but mutability itself does not
make a new type comparable.

### 1.10 Globals

Top-level `let` bindings follow the same rebindability and capability rules as
local `let` bindings. A function may rebind a top-level binding, and all
functions must subsequently observe the new value. Mutating an object through
a top-level `*Foo` must likewise affect the one shared allocation.

The current LIR/backend path does not yet satisfy this contract: it represents
each top-level initializer as a `<global>` pseudo-function and rematerializes
the initializer independently in functions. This feature must not ship while
that behavior can duplicate mutable global objects.

Implement real target-neutral global storage as part of this work:

- give `LirProgram` an ordered global table with stable IDs, runtime-erased
  types, and initializer bodies;
- add explicit global load and global store operations rather than caching an
  initializer in each function's local binding map;
- run initializers once in source order before `main` executes;
- initialize the module-state storage before evaluating any initializer so an
  initializer may call a function that accesses an earlier global;
- retain the existing diagnostic for a forward global whose type is not yet
  known unless a separate declaration mechanism is added later.

For Naravm, lower module state to an existing GC container with separate value
and reference slots. Reserve a callee-saved reference register for that module
state, remove it from the ordinary register allocator, initialize it at the
entrypoint, and use existing `getvat`/`setvat` and `getrfat`/`setrfat`
instructions for global loads and stores. This is VL-side integration only;
do not modify the Naravm checkout or add target concepts to HIR/LIR.

If a module has no `main`, retain its initializer metadata in LIR and emitted
module structure for future library loading, but do not invent a second entry
point policy in this feature.

### 1.11 Explicit non-goals

This work does not add:

- ownership, moves, borrowing, lifetimes, or exclusive aliases;
- freezing, snapshots, deep copies, or copy-on-write;
- raw pointers, addresses, dereference expressions, pointer arithmetic, or
  nullable references;
- concurrency or data-race guarantees;
- fixed local bindings (`const` may be considered later if demonstrated);
- mutable primitive references such as `*u64`;
- reference-kind generic bounds or `*T`;
- methods, setters, operator overloading, or user-defined coercions.

## 2. Surface grammar

The lexer already produces `TokenKind::Star` for `*`, so no token or keyword
is needed. Its meaning is determined by parser position: prefix capability in
a type and multiplication in an expression.

Update the syntax grammar to:

```text
type          := mutable_type | type_atom
mutable_type  := "*" type_atom
type_atom     := "u64" | "i64" | "f64" | "bool" | "u8"
               | "String" | "File" | ident
               | "Array" "[" type "]"
               | "void"

param         := ident ":" type
let_item      := "let" ident (":" type)? "=" expr ";"
let_stmt      := "let" ident (":" type)? "=" expr ";"
function_item := "fun" ident type_params? "(" params? ")"
               (":" type)? block
```

The recursive `type` inside `Array[...]` permits `Array[*Foo]`. Parsing
`mutable_type` through `type_atom`, rather than through `type`, makes `**Foo`
an immediate, focused error. Type spans include the leading `*`.

Update parser recovery so a malformed type such as `*;` emits one type error
and recovers at the existing declaration boundary. Preserve expression
parsing of `a * b` and unary-expression diagnostics unchanged.

## 3. Compiler representation

### 3.1 `vl-common`

Add one source-level capability form to `VlType`:

```rust
pub enum VlType {
    // existing variants
    Mutable(Box<VlType>),
}
```

The name should describe language semantics (`Mutable` or `MutableView`), not
implementation (`Pointer`). Required helpers:

- `is_reference_type()` for `String`, `File`, `Object`, and `Array`;
- `is_mutable_view()`;
- `readonly_view()` to remove one outer mutable capability;
- `runtime_type()` or `erase_capability()` to recursively remove capability
  qualifiers before LIR/codegen;
- recursive `is_void`, array-element, display, and validation behavior;
- a well-formedness check rejecting mutable scalar/void/nested/unconstrained
  parameter forms.

`Display` must round-trip the surface spelling (`*Foo`,
`*Array[*Foo]`). Module catalog signatures already use `VlType`, so they gain
capability information without a parallel signature structure.

### 3.2 AST and HIR

The AST and HIR do not need separate mutation flags. Their existing
`Option<VlType>` annotations, parameter types, field types, return types, and
type arguments carry the qualifier recursively.

HIR lowering must preserve every qualified `VlType` exactly. Do not erase
capability during AST-to-HIR lowering: type checking still needs it for field
and index writes, calls, returns, generics, and diagnostics.

### 3.3 Type-checker type

Mirror the source form in `vl-typecheck::Ty`:

```rust
pub enum Ty {
    // existing variants
    Mutable(Box<Ty>),
}
```

Update all structural operations, including:

- `Ty::from_vl` and `Ty::from_vl_in`;
- `Display`;
- `is_concrete` and normalized-type validation;
- `subst_ty`;
- `mangle_ty`;
- integer-defaulting and poison traversal helpers;
- array-element and object-base helpers;
- generic constraint collection, solving, and unification;
- monomorphization instance validation;
- all exhaustive matches in tests and downstream crates.

Keep capability in `TypedProgram` through the end of type checking. Add a
separate runtime-erasure helper rather than teaching ordinary type equality to
forget it.

### 3.4 Directional compatibility

Replace the current broadly used exact `types_compatible` concept with
operations whose direction is explicit:

- `same_type(a, b)` for invariant/equality checks;
- `can_coerce(got, want)` for initializer, assignment, argument, return,
  field, and element boundaries;
- `common_type(a, b)` for array literals and generic constraint merging;
- `project_capability(receiver, member)` for field and element reads;
- `runtime_type(ty)` for LIR/backend representation.

`can_coerce(*R, R)` is true for the same reference shape. `can_coerce(R, *R)`
is false. Nested generic/container arguments remain invariant initially; the
projection rule provides transitive read-only access without introducing
general variance in this change.

Do not use directional coercion as a substitute for symmetric operand checks.
For example, arithmetic remains exact after numeric-literal coercion, while a
read-only comparison may explicitly inspect the read-only base type.

## 4. Work by compiler stage

### 4.1 `vl-lex`

No lexer implementation change is expected because `*` already has a token.

Work:

1. Update `crates/vl-lex/GRAMMAR.md` to mention both type-qualifier and
   multiplication contexts without assigning semantics in the lexer.
2. Add a regression test proving `*Foo` tokenizes as `Star Ident` and
   `a * b` remains the same token sequence.
3. Confirm no `var`, `val`, `const`, `mut`, `&`, or dereference token is added.

### 4.2 `vl-syntax`

Work:

1. Extend `parse_type` to accept prefix `*` in every type position: local and
   top-level annotations, parameters, results, object fields, array element
   types, and explicit generic arguments.
2. Preserve a span covering the qualifier and complete inner type.
3. Reject obvious invalid forms (`*u64`, `*bool`, `*void`, `**Foo`) with one
   Ariadne diagnostic and normal recovery. Leave constraints requiring generic
   substitution to type checking.
4. Keep multiplication parsing unchanged.
5. Update `crates/vl-syntax/GRAMMAR.md`, its AST description, examples,
   recovery notes, and error table.

Tests:

- `*Foo`, `*Array[u64]`, `Array[*Foo]`, and `*Array[*Foo]` in every declaration
  position;
- mutable type arguments in `f::[*Foo](x)`;
- invalid scalar, void, nested-star, missing-inner-type, and unknown-type cases;
- multiplication next to annotated mutable types;
- recovery continues to later items/statements after each malformed type.

### 4.3 `vl-semantic`

Name resolution should distinguish parameters from ordinary definitions so it
can enforce fixed parameter bindings before type checking.

Work:

1. Extend `DefKind` with `Parameter` (and use explicit kinds when interning
   definitions rather than treating everything as `Local`).
2. On a direct `Stmt::Assign` target, resolve the name as today, then emit one
   diagnostic if the definition is a parameter.
3. Still resolve the right-hand side and retain the target mapping so later
   stages can remain structurally complete without cascading errors.
4. Do not reject `parameter.field = ...` or `parameter[index] = ...` here;
   those mutate a referent and are decided by the parameter's type in
   `vl-typecheck`.
5. Preserve ordinary local and top-level rebinding.

Tests:

- direct reassignment of primitive, read-only-reference, and mutable-reference
  parameters;
- field/index mutation through a parameter continues to resolve;
- local shadowing and assignment behavior remains unchanged;
- one root diagnostic for an unresolved or duplicated parameter target.

### 4.4 `vl-hir`

Work:

1. Preserve `VlType::Mutable` recursively in object definitions, lets,
   parameters, results, and call type arguments.
2. Preserve the existing distinction between binding assignment,
   `FieldAssign`, and `IndexAssign`; do not desugar them into pointer
   operations.
3. Keep parameter bindings structurally fixed by relying on the semantic
   diagnostic; HIR does not need an assignability flag unless a later pass
   needs it for internal validation.
4. Update HIR comments so references are capabilities, not copied object
   values or raw pointers.

Tests should assert qualified types survive lowering and resolved definition
IDs still point to the same parameter/local definitions.

### 4.5 `vl-typecheck`

This is the main semantic implementation.

Declaration and inference work:

1. Validate every qualified type recursively and poison invalid annotations.
2. Infer uncontextualized object/array allocations as read-only.
3. When an expected mutable type is present, type fresh object/array
   allocations with that mutable capability.
4. Never use expected context to upgrade variables, fields, index results, or
   function calls.
5. Fix a binding's type after initialization; later reassignments use
   directional coercion.

Expression and mutation work:

1. Permit field reads through `Object` and `Mutable(Object)`.
2. Permit field writes only when the base expression has
   `Mutable(Object(...))`.
3. Permit array reads through `Array[T]` and `Mutable(Array[T])`.
4. Permit element writes only through `Mutable(Array[T])`.
5. Apply `project_capability` to reference-valued fields and elements.
6. Check the stored field/element declaration type, not the projected read
   type, when writing through a mutable receiver.
7. Keep `String`/`File` operations governed by their extern signatures; do not
   infer mutation from target implementation details.

Boundary work:

1. Use `can_coerce` for annotated lets, rebinding assignment, function
   arguments, returns, extern arguments, object literals, array literals, and
   array stores.
2. Retain the actual expression capability in `TypedProgram`; an accepted
   downgrade needs no synthetic HIR node or runtime instruction.
3. Give capability failures focused diagnostics rather than generic shape
   mismatches where possible.
4. Keep poison behavior: one illegal mutation/upgrade reports once and records
   `Ty::Error` so downstream passes stay quiet.

Generic work:

1. Recurse through `Mutable` in substitution and normalization.
2. Allow `*Foo` as a concrete explicit or inferred `T`.
3. Reject `*T` in a declared type until a reference-kind bound exists.
4. Teach constraint collection that a concrete read-only formal may accept a
   mutable actual by downgrade.
5. Make repeated `T` constraints choose the read-only common capability when
   combining `Foo` and `*Foo`.
6. Include capability in instance keys and mangled names to avoid conflating
   source-level signatures, even though LIR later erases it.
7. Re-run bound checking after substitution so a prohibited mutable/scalar
   shape cannot reach LIR.

Representative tests:

- legal `*Foo -> Foo` at every boundary and illegal reverse flow;
- readonly aliases observe writes made through mutable aliases;
- local rebinding works for `Foo`, `*Foo`, primitives, and arrays;
- read-only object field and array element assignments fail;
- mutable object field and array element assignments pass;
- deep projections through nested objects/arrays do not leak mutability;
- mutable return types preserve authority and read-only results cannot be
  laundered;
- fresh literal contextual typing and read-only defaulting;
- mutable aliases as generic type arguments, explicit turbofish, inference,
  substitution, forwarding, and monomorphization;
- mixed mutable/read-only generic constraints resolve to read-only;
- invalid `*` shapes are poisoned once;
- numeric literal coercion, casts, control flow, and definite-return behavior
  remain unchanged.

### 4.6 `vl-lir`

Reference capability is compile-time-only and must not create a new register
class or instruction family.

Work:

1. Erase capability recursively when producing LIR function signatures,
   object layouts, array element metadata, instruction result metadata, and
   global metadata.
2. Add a boundary validation assertion/diagnostic ensuring no `Ty::Mutable`,
   unresolved `Param`, `Int`, or `Error` reaches backend emission.
3. Continue lowering accepted mutable field/element writes to `ObjectSet` and
   `ArraySet`; rejected writes never reach LIR.
4. Preserve ordinary local rebinding as `Copy` into the binding's home
   register. Ensure object and array reference aliases get independent homes
   so rebinding one local does not rebind another alias.
5. Replace rematerialized pseudo-globals with the global table and explicit
   global load/store operations described in section 1.10.
6. Keep LIR target-neutral: global operations describe semantics, not Naravm
   registers or containers.

The human-readable dump should display runtime types, since capabilities have
already served their purpose. If source-level debugging later needs them, add
a typed-HIR dump rather than leaking frontend qualifiers into runtime IR.

Tests:

- readonly and mutable signatures lower to identical runtime layouts;
- accepted object/array mutations emit the existing set instructions;
- rebinding still emits `Copy` and alias homes remain independent;
- globals initialize once, load/store by stable ID, and retain aliases across
  function boundaries;
- no capability wrapper survives the LIR normalization check.

### 4.7 `vl-codegen`

The dummy, stack VM, and Naravm backends must all consume the revised LIR.

Work:

1. Keep `Foo` and `*Foo` on the same target reference representation; no ABI
   change is needed for capability alone.
2. Make target type mapping defensive: if a capability-qualified `Ty` leaks
   past LIR erasure, report an internal compiler diagnostic rather than
   silently choosing a different representation.
3. Render target-neutral global loads/stores in the dummy and stack backends.
4. Implement Naravm module state using its existing container instructions and
   one reserved callee-saved reference register, as described in section
   1.10. Audit register allocation, spills, recursion, user calls, and extern
   calls so the reserved register is never overwritten.
5. Keep all Naravm-specific state layout and opcode selection inside
   `vl-codegen`; do not modify `vl-lir`, the driver, or the Naravm checkout
   based on a target name.
6. Audit `vl_codegen::modules()` and every target-specific module catalog.
   Mark extern parameters/results mutable only when the language-visible API
   grants mutation authority; do not infer it from hidden VM implementation
   state.

Backend tests:

- identical ABI/register class for `Foo` and `*Foo`;
- object and array mutation via mutable parameters;
- read-only programs remain bytecode-compatible apart from unrelated global
  initialization changes;
- shared mutable global observed from two functions;
- global rebinding of both value-register and reference-register types;
- nested calls and recursion preserve module state;
- target capability failures still use Ariadne diagnostics returned to the
  driver.

### 4.8 Driver

The driver should require little semantic code, but it owns integration and
diagnostic emission.

Work:

1. Keep the pipeline order unchanged and emit all new diagnostics through
   `emit_all`.
2. Update `main` signature validation only as needed for exhaustive
   `VlType` matching; `main` remains zero-argument and `void`.
3. Ensure `--emit ast` shows source capabilities and `--emit lir` shows
   runtime-erased types.
4. Keep target lookup and target-specific module catalogs unchanged in shape.
5. Add CLI smoke cases for one valid mutable program and one rejected
   read-only mutation.

## 5. Diagnostics

All diagnostics remain `vl_common::Diagnostic` and are printed only by the
driver. Suggested new codes, subject to the repository's final error-code
allocation:

- `E106`: invalid mutable-view type (`*u64`, `*void`, `**Foo`, `*T`);
- `E205`: attempted parameter rebinding;
- `E310`: mutation through a read-only object/array view.

Existing boundary mismatch codes should remain where users already expect
them:

- `E306` for an argument capability mismatch;
- `E307` for a return capability mismatch;
- `E309` for initializer/rebinding/field/element incompatibility;
- `E500`-class diagnostics for capability or unresolved types leaking across
  an internal phase boundary.

Diagnostic requirements:

```text
cannot assign field `value` through read-only view `Foo`
  label: this expression has read-only type `Foo`
  note: use a `*Foo` parameter or binding when this function must mutate it
```

```text
cannot pass read-only `Foo` to mutable parameter `foo: *Foo`
  label: mutation authority is required here
  note: a read-only view cannot be upgraded to `*Foo`
```

```text
cannot rebind parameter `foo`
  label: parameters are fixed bindings
  note: `foo: *Foo` permits field mutation, not assignment to `foo`
```

Prefer one root-cause diagnostic. After an invalid type or capability flow,
poison the affected node/binding and suppress downstream mutation, call, LIR,
and backend errors.

## 6. Migration of the repository

Existing scalar-only code remains source-compatible. Existing object and array
writes need explicit mutable views.

### Objects

Before:

```vl
fun bump(counter: Counter): Counter {
    counter.value = counter.value + 1;
    return counter;
}

let counter = Counter { value = 1, label = "count" };
```

After:

```vl
fun bump(counter: *Counter): *Counter {
    counter.value = counter.value + 1;
    return counter;
}

let counter: *Counter = Counter { value = 1, label = "count" };
```

Read-only object functions keep the unstarred type.

### Arrays

Before:

```vl
let scores: Array[u64] = Array.new(3);
scores[0] = 10;
```

After:

```vl
let scores: *Array[u64] = Array.new(3);
scores[0] = 10;
```

Read-only algorithms remain unstarred:

```vl
fun sum(scores: Array[u64], count: u64): u64;
```

Mutating algorithms become explicit:

```vl
fun fill(scores: *Array[u64]);
```

Update every affected file under `examples/`, inline Rust test source,
`README.md`, grammar documents, website snippets, and stdlib examples. Add a
dedicated successful example showing alias visibility and an `err_*.vl`
example showing read-only mutation rejection.

## 7. Website and service surfaces

### Website

Update:

- `website/src/pages/docs/basics.md`: one `let`, all locals rebindable, and
  capability syntax;
- `website/src/pages/docs/functions.md`: fixed parameters, mutable parameters,
  and capability-preserving results;
- `website/src/pages/docs/objects.md`: aliasing, read-only views, mutable views,
  transitive projection, and no borrowing/freezing;
- `website/src/pages/docs/arrays.md`: `Array[T]` reads versus
  `*Array[T]` writes, constructor inference, and generic examples;
- module/stdlib pages and `website/src/data/stdlib.yaml` if an extern
  signature changes;
- landing-page and playground sample programs that mutate arrays/objects;
- the TextMate grammar so `*` in a type receives a capability/type-qualifier
  scope while multiplication remains an operator.

The browser client does not implement VL semantics; it sends source to `vlc`.
Only examples, highlighting, and expected diagnostics should change there.

Validate with system `pnpm` directly:

```sh
cd website
pnpm check
pnpm build
```

### `vlc`

`vlc` delegates to the `vl` binary, so its request/response schema needs no
change. Add or update Go integration fixtures to ensure:

- valid `*Foo` source compiles through `/v1/compile`;
- an illegal read-only mutation returns HTTP 422 with compiler diagnostics;
- ANSI stripping and response limits still work with the new messages.

Do not duplicate capability rules in Go or TypeScript.

## 8. Test matrix and acceptance criteria

The feature is complete only when all of the following hold.

### Binding behavior

- Local `let` bindings of primitive, read-only reference, and mutable
  reference types can be rebound to compatible values.
- Rebinding never changes another alias's binding.
- Parameters of every type reject direct rebinding.
- A `*Foo` parameter may mutate `Foo`; a `Foo` parameter may not.

### Capability flow

- `*R -> R` succeeds at all typed boundaries without runtime code.
- `R -> *R` fails at all typed boundaries with one focused diagnostic.
- A function cannot launder a read-only parameter/global/field into a mutable
  result.
- `as` cannot upgrade a capability.

### Aliasing and projection

- Read-only aliases observe mutations performed through mutable aliases.
- Multiple mutable aliases refer to the same allocation.
- Read-only object and array receivers cannot be written through.
- Nested mutable fields/elements are downgraded when reached through a
  read-only receiver.
- Reaching the same nested value through a mutable receiver preserves its
  declared mutable capability.

### Inference and generics

- Fresh objects/arrays default read-only without context.
- Expected mutable contexts make fresh allocations mutable.
- Existing read-only expressions never upgrade from expected context.
- Generic inference and explicit type arguments preserve mutable references.
- Mixed mutable/read-only constraints select the safe read-only common type.
- Every monomorphized instance is normalized before LIR.

### Runtime and globals

- Capability qualifiers do not change object layout, GC identity, register
  class, calling convention, or emitted field/array operation.
- Top-level initialization occurs exactly once.
- Global reads, rebinding, and referent mutation are visible across functions
  and survive nested calls/recursion.
- No target-specific global representation escapes `vl-codegen`.

### Recovery and pipeline quality

- Lex/parse/resolve/typecheck continue after independent errors.
- Invalid capability nodes poison downstream work without cascades or panic.
- All internal matches are exhaustive and no user input path uses `unwrap()`.
- Existing numeric, control-flow, module, generic, LIR, and backend regression
  suites remain green after intentional source migrations.

Run the repository gate and the additional surfaces:

```sh
./scripts/check.sh
cargo test -p vl-lex
cargo test -p vl-syntax
cargo test -p vl-semantic
cargo test -p vl-hir
cargo test -p vl-typecheck
cargo test -p vl-lir
cargo test -p vl-codegen
cargo test --test pipeline
(cd vlc && go test ./...)
(cd website && pnpm check && pnpm build)
```

Review any `tests/golden/*.lir` change manually. Capability-only changes should
normally disappear before LIR; global-storage work is the expected reason for
larger intentional golden changes.

## 9. Implementation sequence

Follow workspace dependency order and keep commits stage-focused where
possible.

1. **Language contract and common types**
   - Land this plan and the `VlType` capability representation/helpers.
   - Add common type-formatting and well-formedness tests.
2. **Lexer and parser**
   - Parse qualified types in every position, update both grammar documents,
     and add recovery tests.
3. **Resolution**
   - Distinguish parameter definitions and reject parameter rebinding.
4. **HIR**
   - Preserve qualified types and update lowering tests/comments.
5. **Type checking**
   - Implement directional coercion, fresh-allocation context, mutation
     checks, transitive projection, diagnostics, and poison behavior.
6. **Generics and monomorphization**
   - Extend constraints, substitution, bounds, mangling, worklists, and
     normalized-instance validation.
7. **LIR runtime erasure and globals**
   - Erase capabilities, add boundary validation, and replace pseudo-globals
     with target-neutral global operations.
8. **Backends**
   - Update all target matches, implement global operations, and add Naravm
     module-state tests without editing Naravm.
9. **Driver and end-to-end migration**
   - Update examples, inline fixtures, pipeline tests, smoke checks, and
     reviewed goldens.
10. **Website and compiler service**
    - Update language/stdlib documentation, highlighting, playground samples,
      and service integration tests.
11. **Final audit**
    - Search for every exhaustive `VlType`/`Ty` match, every object/array
      write, every function signature, and every documented example; run all
      gates listed above.

Suggested conventional commit scopes follow the stage being changed, for
example `feat(vl-common): model mutable reference views`,
`feat(vl-typecheck): enforce reference capabilities`, and
`feat(vl-codegen): lower shared module globals`.

## 10. Canonical example

The completed language should accept:

```vl
use std;

type Counter = object {
    value: u64,
};

fun read(counter: Counter): u64 {
    return counter.value;
}

fun increment(counter: *Counter) {
    counter.value = counter.value + 1;
}

fun main() {
    let counter: *Counter = Counter { value = 0 };
    let view: Counter = counter;

    increment(counter);
    std.print_u64(read(view)); // prints 1

    counter = Counter { value = 10 }; // local let rebinding is allowed
    increment(counter);
    std.print_u64(read(counter)); // prints 11
}
```

It should reject each invalid operation independently:

```vl
fun invalid(view: Counter, editable: *Counter) {
    view.value = 1;       // error: read-only view
    view = editable;      // error: parameters cannot be rebound

    let readonly: Counter = editable;
    let upgrade: *Counter = readonly; // error: capability upgrade
}
```

This is the intended center of gravity: one simple binding form, fixed
parameters, read-only reference APIs by default, explicit mutation authority,
ordinary GC aliasing, and no borrow checker.
