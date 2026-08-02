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
