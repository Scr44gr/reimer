//! Primitive bit-preserving conversions used by `std::binary`.

/// ABI symbol for reconstructing an `f32` from IEEE-754 bits.
pub const BINARY_F32_FROM_BITS_SYMBOL: &str = "binary_f32_from_bits";
/// ABI symbol for extracting the IEEE-754 bits of an `f32`.
pub const BINARY_F32_TO_BITS_SYMBOL: &str = "binary_f32_to_bits";
/// ABI symbol for reconstructing an `f64` from IEEE-754 bits.
pub const BINARY_F64_FROM_BITS_SYMBOL: &str = "binary_f64_from_bits";
/// ABI symbol for extracting the IEEE-754 bits of an `f64`.
pub const BINARY_F64_TO_BITS_SYMBOL: &str = "binary_f64_to_bits";

#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn binary_f32_from_bits(bits: u32) -> f32 {
    f32::from_bits(bits)
}

#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn binary_f32_to_bits(value: f32) -> u32 {
    value.to_bits()
}

#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn binary_f64_from_bits(bits: u64) -> f64 {
    f64::from_bits(bits)
}

#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn binary_f64_to_bits(value: f64) -> u64 {
    value.to_bits()
}

#[cfg(test)]
mod tests {
    use super::{
        binary_f32_from_bits, binary_f32_to_bits, binary_f64_from_bits, binary_f64_to_bits,
    };

    #[test]
    fn conversions_should_preserve_every_bit() {
        let f32_bits = 0x7fc0_1234;
        assert_eq!(binary_f32_to_bits(binary_f32_from_bits(f32_bits)), f32_bits);

        let f64_bits = 0x7ff8_0000_0000_1234;
        assert_eq!(binary_f64_to_bits(binary_f64_from_bits(f64_bits)), f64_bits);
    }
}
