# Methods, traits, and generics

Reimer monomorphizes generic code on demand. Calls use static dispatch; there is no hidden virtual dispatch in the experimental trait model.

## Inherent methods

```reimer
struct Counter {
    value: i32,
}

impl Counter {
    fn new(value: i32) -> Counter {
        Counter { value: value }
    }

    fn add(&mut self, amount: i32) {
        self.value += amount;
    }

    fn get(&self) -> i32 {
        self.value
    }
}
```

Associated functions use `Type::function()`. Receiver methods use `value.method()`.

## Generic types and functions

```reimer
struct Holder<T> {
    value: T,
}

impl<T> Holder<T> {
    fn new(value: T) -> Holder<T> {
        Holder { value: value }
    }
}

fn first<T, const N: usize>(values: [T; N]) -> T {
    values[0]
}
```

Type arguments can often be inferred from arguments or an expected return type. Explicit arguments use angle brackets, such as `Holder<i32>` and `tensor<f32, 2>`.

## Variadic type packs

A final `...Types` parameter accepts zero or more types. Packs are
monomorphized with the rest of the generic declaration; they do not create a
runtime list or erase type information.

```reimer
struct Bundle<...Types> {
    values: (...Types),
}

fn forward<...Types>(values: (...Types)) -> Bundle<...Types> {
    Bundle { values: values }
}
```

A pack can be inferred from a tuple pattern or supplied explicitly. Every type
in a bounded pack must satisfy the bound:

```reimer
fn preserve<...Types: Copy>(values: (...Types)) -> (...Types) {
    values
}
```

Use `...Pack => Template<Pack>` to map every pack element through a type or
value template. A value expansion is valid directly inside a tuple or array.

```reimer
struct Slot<T> {
    value: i32,
}

fn make_slot<T>() -> Slot<T> {
    Slot { value: 0 }
}

fn slots<...Types>() -> (...Types => Slot<Types>) {
    (...Types => make_slot<Types>(),)
}
```

The compiler requires a source-level pack to be the final parameter in its
generic list. An implementation pack and a method's own generic parameters are
separate lists, so a variadic type may still expose generic methods.

## Type-addressable tuples

Heterogeneous libraries can select tuple fields by their concrete type. These
operations are compile-time field selection, not runtime reflection:

```reimer
let mut values: (i32, bool, u8) = (40, true, 7);
{
    let selected: (&mut i32, &bool) =
        values.split_type_mut<i32, bool>();
    *selected.0 += if *selected.1 { 2 } else { 0 };
};
let answer = *values.get_type<i32>();
```

- `get_type<T>()` returns `&T`.
- `get_type_mut<T>()` returns `&mut T`.
- `split_type_mut<Write, Read...>()` returns one exclusive reference followed
  by disjoint shared references in a tuple.
- `assert_unique_types()` verifies that every tuple element type is unique.

Missing, duplicate, and repeated requested types are compile-time errors. The
split operation records one scoped borrow of the source tuple and lowers to
constant field addresses, which makes it suitable for static heterogeneous
registries and ECS queries.

## Traits and bounds

```reimer
trait Measure {
    fn measure(&self) -> i32;
}

fn read<T: Measure>(value: &T) -> i32 {
    value.measure()
}
```

Bounds may appear on parameters or in a `where` clause. Implementations are checked for missing or incompatible methods and for coherence conflicts.

## Formatting traits

Implement `std::fmt::Display` to allow `{value}` interpolation and `std::fmt::Debug` to allow `{value:?}`.

```reimer
from std::fmt import Display, FormatError, Formatter;

struct Health {
    current: u32,
}

impl Display for Health {
    fn fmt(&self, formatter: &mut Formatter) -> Result<(), FormatError> {
        formatter.write_u128(self.current as u128)
    }
}
```

The compiler selects the implementation statically. A nominal type without the required trait produces a diagnostic before code generation.

## Structural and marker derives

The compiler provides `Copy`, `Clone`, `Debug`, `Eq`, `Hash`, and `Default`
when every stored field satisfies the corresponding structural rules.

```reimer
@derive(Copy, Clone, Debug, Eq, Hash, Default)
struct Cell {
    row: u32,
    column: u32,
}
```

Derived `Clone` cannot hide an allocation or allocator choice. Owned standard-library types provide explicit duplication operations instead.

Libraries can also define zero-method, non-generic marker traits. Listing one
in `@derive` declares the marker implementation after the compiler verifies
its supertraits:

```reimer
trait Component: Copy + Send + Sync {}

@derive(Copy, Component)
struct Velocity {
    x: f32,
    y: f32,
}
```

A behavioral trait cannot be derived this way. Implement it explicitly so its
methods, costs, and failure behavior remain visible.
