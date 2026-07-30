//! Scalar mathematics ABI used by `std::math`.

/// ABI symbol for `f32` absolute value.
pub const MATH_ABSOLUTE_F32_SYMBOL: &str = "math_absolute_f32";
/// ABI symbol for `f64` absolute value.
pub const MATH_ABSOLUTE_F64_SYMBOL: &str = "math_absolute_f64";
/// ABI symbol for `f32` square root.
pub const MATH_SQRT_F32_SYMBOL: &str = "math_sqrt_f32";
/// ABI symbol for `f64` square root.
pub const MATH_SQRT_F64_SYMBOL: &str = "math_sqrt_f64";
/// ABI symbol for `f32` floor.
pub const MATH_FLOOR_F32_SYMBOL: &str = "math_floor_f32";
/// ABI symbol for `f64` floor.
pub const MATH_FLOOR_F64_SYMBOL: &str = "math_floor_f64";
/// ABI symbol for `f32` ceiling.
pub const MATH_CEIL_F32_SYMBOL: &str = "math_ceil_f32";
/// ABI symbol for `f64` ceiling.
pub const MATH_CEIL_F64_SYMBOL: &str = "math_ceil_f64";
/// ABI symbol for `f32` rounding.
pub const MATH_ROUND_F32_SYMBOL: &str = "math_round_f32";
/// ABI symbol for `f64` rounding.
pub const MATH_ROUND_F64_SYMBOL: &str = "math_round_f64";
/// ABI symbol for `f32` sine.
pub const MATH_SIN_F32_SYMBOL: &str = "math_sin_f32";
/// ABI symbol for `f64` sine.
pub const MATH_SIN_F64_SYMBOL: &str = "math_sin_f64";
/// ABI symbol for `f32` cosine.
pub const MATH_COS_F32_SYMBOL: &str = "math_cos_f32";
/// ABI symbol for `f64` cosine.
pub const MATH_COS_F64_SYMBOL: &str = "math_cos_f64";
/// ABI symbol for `f32` tangent.
pub const MATH_TAN_F32_SYMBOL: &str = "math_tan_f32";
/// ABI symbol for `f64` tangent.
pub const MATH_TAN_F64_SYMBOL: &str = "math_tan_f64";
/// ABI symbol for the natural `f32` exponential.
pub const MATH_EXP_F32_SYMBOL: &str = "math_exp_f32";
/// ABI symbol for the natural `f64` exponential.
pub const MATH_EXP_F64_SYMBOL: &str = "math_exp_f64";
/// ABI symbol for the natural `f32` logarithm.
pub const MATH_LN_F32_SYMBOL: &str = "math_ln_f32";
/// ABI symbol for the natural `f64` logarithm.
pub const MATH_LN_F64_SYMBOL: &str = "math_ln_f64";
/// ABI symbol for `f32` exponentiation.
pub const MATH_POW_F32_SYMBOL: &str = "math_pow_f32";
/// ABI symbol for `f64` exponentiation.
pub const MATH_POW_F64_SYMBOL: &str = "math_pow_f64";

macro_rules! unary_math_function {
    ($name:ident, $type:ty, $operation:ident) => {
        #[must_use]
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(value: $type) -> $type {
            value.$operation()
        }
    };
}

macro_rules! binary_math_function {
    ($name:ident, $type:ty, $operation:ident) => {
        #[must_use]
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(value: $type, operand: $type) -> $type {
            value.$operation(operand)
        }
    };
}

unary_math_function!(math_absolute_f32, f32, abs);
unary_math_function!(math_absolute_f64, f64, abs);
unary_math_function!(math_sqrt_f32, f32, sqrt);
unary_math_function!(math_sqrt_f64, f64, sqrt);
unary_math_function!(math_floor_f32, f32, floor);
unary_math_function!(math_floor_f64, f64, floor);
unary_math_function!(math_ceil_f32, f32, ceil);
unary_math_function!(math_ceil_f64, f64, ceil);
unary_math_function!(math_round_f32, f32, round);
unary_math_function!(math_round_f64, f64, round);
unary_math_function!(math_sin_f32, f32, sin);
unary_math_function!(math_sin_f64, f64, sin);
unary_math_function!(math_cos_f32, f32, cos);
unary_math_function!(math_cos_f64, f64, cos);
unary_math_function!(math_tan_f32, f32, tan);
unary_math_function!(math_tan_f64, f64, tan);
unary_math_function!(math_exp_f32, f32, exp);
unary_math_function!(math_exp_f64, f64, exp);
unary_math_function!(math_ln_f32, f32, ln);
unary_math_function!(math_ln_f64, f64, ln);
binary_math_function!(math_pow_f32, f32, powf);
binary_math_function!(math_pow_f64, f64, powf);

#[cfg(test)]
mod tests {
    use super::{math_cos_f64, math_pow_f32, math_sin_f64, math_sqrt_f32};

    #[test]
    fn scalar_math_should_preserve_expected_precision() {
        assert!((math_sqrt_f32(81.0) - 9.0).abs() < f32::EPSILON);
        assert!((math_pow_f32(3.0, 4.0) - 81.0).abs() < f32::EPSILON);
        assert!(math_sin_f64(0.0).abs() < f64::EPSILON);
        assert!((math_cos_f64(0.0) - 1.0).abs() < f64::EPSILON);
    }
}
