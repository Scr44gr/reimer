# Numeric literals and integer behavior

Reimer supports decimal, hexadecimal, binary, and octal integer literals:

```reimer
let decimal = 1_000_000;
let hexadecimal: u32 = 0xDEAD_BEEF;
let binary = 0b1010_0110;
let permissions = 0o755;
```

The base prefixes are `0x` or `0X`, `0b` or `0B`, and `0o` or `0O`.
Hexadecimal digits are case-insensitive.

## Separators

One `_` may separate two digits in any integer base and in each decimal
floating-point component:

```reimer
let distance = 12_500;
let mask = 0xFF_00_FF_00;
let fraction = 1_024.5_0e2;
```

A separator cannot begin or end a digit sequence, touch a base prefix, or
appear next to another separator. Forms such as `123_`, `1__000`, `0x_FF`, and
`1e_2` are compile-time errors with an exact source span.

## Typing and range

The spelling selects a value, not a storage type. Integer literals are
contextually typed by their destination or surrounding expression and default
to `i32` when no context exists. A value that does not fit the selected type is
rejected. The frontend can parse positive magnitudes through `u128`; the unary
`-` operator supplies negative signed values, including each signed type's
minimum value.

Floating-point literals remain decimal. Hexadecimal floating-point syntax and
numeric type suffixes are intentionally not part of the current language; use
a type annotation or an explicit cast.
