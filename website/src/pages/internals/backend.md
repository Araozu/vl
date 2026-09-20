---
layout: ../../layouts/Docs.astro
title: Backend crates
description: The VL stages from a resolved tree to target output.
eyebrow: Internals
availability: VL 0.1+
---

# Backend crates

The backend stages turn a resolved program into target-neutral code and then
into target output:

```text
vl-hir → vl-typecheck → vl-lir → vl-codegen
```

## `vl-hir`

Lowers the resolved syntax tree into a desugared tree with node ids and
`DefId` links.

## `vl-typecheck`

Checks `u64`, `i64`, `f64`, `bool`, `u8`, `Array[T]`, and byte-string values,
producing typed HIR and diagnostics. Array literals must hold one uniform
element type; indexing requires an `Array[T]` base and a `u64` index.
Generic functions check once with opaque parameters and monomorphize per
concrete call (`f$u64`, ...).

## `vl-lir`

Lowers typed HIR to target-agnostic three-address code. It does not know which
backend will consume the program.

## `vl-codegen`

Defines the `Target` trait and registers `naravm`, `dummy`, and `stackvm`.
`naravm` serializes Naravm 0.2 vmfiles; the other targets are inspection
backends.

The Naravm backend compiles every `function` item: `function main()` (which
takes no parameters) becomes the `<entrypoint>` function and each other user
function becomes its own Nara function: integer (`u64`/`i64`/`u8`) and `f64`
arithmetic, integer equality and ordering (`i64` ordering is emulated by
flipping the sign bit before the unsigned `ltu`), boolean logic, branches, and
`while` loops lower to `lv`/`lrf`, typed arithmetic, `eq`/`ltu`/`xor`,
`jz`/`jmp`, and `calli` for `std.print` / `std.println` / `std.print_u64`
and for calls
between user functions (including recursion). `std.println` lowers to two
`print` calls (the value, then `"\n"`), since the VM has no native newline
operation. A call spills live caller
registers (`pushv`/`pushrf`, including cached comparison temporaries), moves
actuals into the callee slots (`rv11` upwards for values, `rf31` upwards for
`string`/`File` references), emits `calli`, copies the return value out of
`rv11` / `rf31`, then restores the spills; returns do the reverse. `Array[T]`
values are reference values backed by Naravm memory containers: `new` lowers
to `create` (value counts for value elements, ref counts for reference
elements), literals to `createi` plus `setvati`/`setrfati` stores, reads to
`getvat`/`getrfat`, and writes to `setvat`/`setrfat`. At most 15
value and 9 reference parameters per function are supported. Registers are
recycled past their last use so idiomatic programs fit the 32 value
and 32 reference registers; liveness extends across loop back edges so values
used inside a loop keep their registers for the whole loop. Float ordering, string equality, and string
ordering are rejected with diagnostics rather than miscompiled.

User-defined `object` values use the same container representation. The
compiler assigns each field to the value or reference lane, emits `createi`
plus the corresponding immediate field stores, and lowers reads/writes to
`getvati`/`getrfati` and `setvati`/`setrfati`. Object values are passed and
returned through Naravm reference registers, preserving VL's aliasing semantics.
