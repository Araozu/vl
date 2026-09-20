---
layout: ../../layouts/Docs.astro
title: Conditions and loops
description: Use if statements, while loops, break, and continue to control a VL program.
eyebrow: Learn the language
availability: VL 0.1+
---

# Conditions and loops

Programs become useful when they can make a choice or repeat a task. VL uses
`if` for choices and `while` for repetition. Both take a boolean condition in
parentheses.

## Choosing with `if`

The code in the first branch runs when the condition is `true`. An optional
`else` branch runs when it is `false`.

```vl
use std;

function main() {
    let temperature = 25;

    if (temperature > 30) {
        std.println("hot");
    } else {
        std.println("comfortable");
    }
}
```

The condition must be a boolean expression. Comparisons such as `count < 10`
and boolean names such as `ready` produce the values that `if` needs.

You can omit braces when a branch contains one statement, but braces are often
clearer and make it easier to add another statement later:

```vl
use std;

function main() {
    let ready = true;
    if (ready) std.println("go");
}
```

Several choices can be chained with `else if`:

```vl
use std;

function main() {
    let score = 85u64;
    if (score >= 90u64) {
        std.println("A");
    } else if (score >= 80u64) {
        std.println("B");
    } else {
        std.println("keep practicing");
    }
}
```

The complete example keeps the decision and its input together so you can run it
as written.

## Repeating with `while`

`while` runs its body, checks the condition again, and repeats while the
condition remains true. Change something inside the loop so that it eventually
stops.

```vl
use std;

function main() {
    let i = 1;
    while (i <= 3) {
        std.print_u64(i);
        i = i + 1;
    }
}
```

This prints the numbers 1, 2, and 3. The assignment is important: without
`i = i + 1`, the condition would stay true forever.

## `break` and `continue`

`break` leaves the nearest loop immediately. `continue` skips the rest of the
current iteration and checks the loop condition again.

```vl
use std;

function main() {
    let i = 0;
    while (true) {
        i = i + 1;
        if (i == 3) {
            break;
        }
        if (i == 2) {
            continue;
        }
        std.print_u64(i);
    }
}
```

Both keywords only make sense inside a loop. VL reports an error if they are
used in a function that is not currently looping.

## Combining conditions

Use parentheses to make a complicated condition easy to read. `&&` requires
both sides to be true; `||` requires at least one side to be true.

```vl
function main() {
    let logged_in = true;
    let has_permission = true;
    if (logged_in && has_permission) {
        // Open the settings view here.
    }
}
```

The left side of `&&` and `||` is evaluated first. This is useful when the
second part should only run after a first check succeeds.

Continue with [functions](/docs/functions).
