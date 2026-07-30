# Mathematics API

`std::math` provides scalar floating-point functions and compact
single-precision vectors. Its native scalar ABI is private to the standard
library, so application code does not need `unsafe`.

## Scalars

The unsuffixed functions accept and return `f32`, which is the default precision
for rendering, simulation, and tensor data:

- `absolute`, `square_root`, `floor`, `ceil`, and `round`
- `sine`, `cosine`, and `tangent`, with angles measured in radians
- `exponential`, `natural_logarithm`, and `power`
- `minimum`, `maximum`, and `clamp`

Every operation also has an `_f64` form, such as `square_root_f64` and
`power_f64`. This explicit spelling avoids implicit precision changes. `PI`,
`TAU`, and `E` are `f32`; `PI_F64`, `TAU_F64`, and `E_F64` provide the
double-precision constants.

The ABI-backed scalar operations preserve Rust's native IEEE 754 results,
including infinities and NaN values. `minimum`, `maximum`, and `clamp` use the
language's ordinary comparisons; comparisons with NaN are false. `clamp` does
not reorder its bounds.

## Vectors

`Vec2`, `Vec3`, and `Vec4` are `Copy` structures with public `f32` components.
They provide:

- `new`, `zero`, and `splat` construction;
- component-wise `add` and `subtract`;
- scalar `scale`;
- `dot`, `length_squared`, `length`, `distance`, and `lerp`;
- recoverable `normalized`, which returns `None` for an exactly zero vector.

`Vec3` additionally provides the right-handed `cross` product. Methods use
explicit names because operator overloading is not part of the current language
contract.

```reimer
from std::math import Vec3;

fn direction() -> Option<Vec3> {
    let vector = Vec3::new(3.0, 4.0, 0.0);
    vector.normalized()
}
```

Bind a temporary to a local before calling a borrowed method on it. The current
borrow checker requires method receivers to be addressable values:

```reimer
let vector = Vec3::new(3.0, 4.0, 0.0);
let length = vector.length();
```

Scalar and vector math performs no allocation and therefore never selects an
allocator implicitly.
