# Tensors

`std::tensor` provides owned, contiguous, row-major tensors with compile-time rank and explicit allocation.

## Create a tensor

```reimer
from std::alloc import general_allocator;
from std::tensor import TensorError, tensor;

fn create_image() -> Result<i32, TensorError> {
    let allocator = general_allocator();
    let shape: [usize; 2] = [4, 4];
    let created: Result<tensor<f32, 2>, TensorError> = tensor::filled(
        &allocator,
        shape,
        0.0,
    );
    let mut image = created?;
    defer image.deinit();

    image[1, 2] = 42.0;
    if image[1, 2] == 42.0 { Ok(42) } else { Ok(0) }
}
```

Construction checks shape multiplication for overflow and reports allocation failure. The tensor stores its shape, row-major strides, length, and contiguous `Vec<T>` storage.

## Checked access

- `get(indices)` returns a copied `Option<T>`.
- `set(indices, value)` returns whether the index was valid.
- `index(indices)` and multidimensional `value[i, j]` panic on an invalid index.
- flat access is available through `get_flat` and `set_flat`.

Use recoverable lookup when indexes come from untrusted or optional input. Use concise indexing when an invalid index is a program invariant violation.

## Views

`TensorView<T, Rank>` borrows read-only storage. `TensorViewMut<T, Rank>` borrows it exclusively.

```reimer
let view = image.view();
let value = view.get([1, 2]);
```

The resolver propagates the borrow through the view and prevents it from outliving or conflicting with the owner. Editor hovers show source-level names such as `TensorViewMut<f32, 2>`.

## Kernels

The experimental API includes:

- `fill`;
- `multiply_scalar` for `f32` tensors;
- `add_into` with explicit output storage;
- `matmul_into` for checked rank-2 multiplication;
- `parallel_for_mut` through the job system.

Kernels that produce a separate value accept an explicit output tensor so allocation and aliasing remain visible.

## When to use tensors

Tensors fit image buffers, dense simulation grids, matrices, vertex attributes, and other multidimensional contiguous data. For a simple growing one-dimensional collection, prefer `Vec<T>`.

See `examples/m7_tensor.reim`, `examples/m7_matmul.reim`, and `examples/m9_tensor_parallel/main.reim` for executable cases.
