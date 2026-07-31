# Control flow and functions

Reimer uses braces for blocks and does not require parentheses around conditions.

## `if` expressions

An `if` can produce a value when every branch has a compatible type.

```reimer
let sign = if value < 0 {
    -1
} else if value > 0 {
    1
} else {
    0
};
```

Conditions must be `bool`; integers do not convert implicitly to booleans.

## `while`

```reimer
let mut remaining = 3;
while remaining > 0 {
    remaining -= 1;
}
```

Use `break` to leave a loop and `continue` to begin the next iteration.

## `for`

Arrays, slices, and supported iterators work with `for`:

```reimer
let values = [10, 20, 12];
let mut total = 0;

for value in values {
    total += value;
}
```

`str.chars()` provides allocation-free Unicode iteration:

```reimer
for scalar in "Aλ🦀".chars() {
    let codepoint = scalar as u32;
}
```

## Value-producing `loop`

`loop` repeats until a `break` supplies its result:

```reimer
let answer = loop {
    if ready() {
        break 42;
    }
};
```

The linter recommends `loop` instead of `while true`.

## `match`

```reimer
fn describe(value: Option<i32>) -> i32 {
    match value {
        Some(number) if number > 0 => number,
        Some(_) => 0,
        None => -1,
    }
}
```

Patterns cover enum variants, tuples, struct-like fields, literal values, and `_`. Guards run only after their pattern matches.

## Functions and returns

```reimer
fn divide_or_zero(value: i32, divisor: i32) -> i32 {
    if divisor == 0 {
        return 0;
    }
    value / divisor
}
```

A trailing expression provides the return value. `return expression;` exits early. Functions that return `()` may omit `-> ()`.

## Deferred cleanup

`defer expression;` schedules cleanup at the end of the current lexical scope. Multiple deferred expressions run in reverse declaration order.

```reimer
let file = open("settings.txt")?;
defer file.deinit();
```

Place `defer` immediately after successful acquisition so every later early return and `?` path is covered.

## Assertions and panic

- `assert(condition)` and `assert(condition, message)` run in every profile.
- `debug_assert` follows the same syntax but is omitted when the selected Reimer profile is optimized.
- `panic(message)` terminates through the runtime panic boundary.

```reimer
assert(width > 0, "width must be positive");
debug_assert(cache_len == values.len(), "cache length is stale");
```

Disabled `debug_assert` operands are not evaluated, so they must not contain required side effects.
