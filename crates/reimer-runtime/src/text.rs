//! Allocation-free formatting and Unicode helpers used by `std::string`.

use std::fmt::{self, Write as _};

/// ABI symbol for single-precision display formatting.
pub const FORMAT_F32_SYMBOL: &str = "text_format_f32";
/// ABI symbol for double-precision display formatting.
pub const FORMAT_F64_SYMBOL: &str = "text_format_f64";
/// ABI symbol for signed 128-bit decimal formatting.
pub const FORMAT_I128_SYMBOL: &str = "text_format_i128";
/// ABI symbol for unsigned 128-bit decimal formatting.
pub const FORMAT_U128_SYMBOL: &str = "text_format_u128";
/// ABI symbol for encoding one Unicode scalar as UTF-8.
pub const UTF8_ENCODE_CHAR_SYMBOL: &str = "utf8_encode_char";
/// ABI symbol for full lowercase Unicode mapping.
pub const UNICODE_LOWERCASE_SYMBOL: &str = "unicode_lowercase";
/// ABI symbol for full uppercase Unicode mapping.
pub const UNICODE_UPPERCASE_SYMBOL: &str = "unicode_uppercase";
/// ABI symbol for Unicode alphabetic classification.
pub const UNICODE_IS_ALPHABETIC_SYMBOL: &str = "unicode_is_alphabetic";
/// ABI symbol for Unicode alphanumeric classification.
pub const UNICODE_IS_ALPHANUMERIC_SYMBOL: &str = "unicode_is_alphanumeric";
/// ABI symbol for Unicode numeric classification.
pub const UNICODE_IS_NUMERIC_SYMBOL: &str = "unicode_is_numeric";
/// ABI symbol for Unicode whitespace classification.
pub const UNICODE_IS_WHITESPACE_SYMBOL: &str = "unicode_is_whitespace";
/// ABI symbol for Unicode lowercase classification.
pub const UNICODE_IS_LOWERCASE_SYMBOL: &str = "unicode_is_lowercase";
/// ABI symbol for Unicode uppercase classification.
pub const UNICODE_IS_UPPERCASE_SYMBOL: &str = "unicode_is_uppercase";
/// ABI symbol for Unicode control classification.
pub const UNICODE_IS_CONTROL_SYMBOL: &str = "unicode_is_control";

const FORMAT_CAPACITY: usize = 64;
const CASE_MAPPING_CAPACITY: usize = 12;
const TEXT_OPERATION_FAILED: isize = -1;

struct StackText<const CAPACITY: usize> {
    bytes: [u8; CAPACITY],
    len: usize,
}

impl<const CAPACITY: usize> StackText<CAPACITY> {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl<const CAPACITY: usize> fmt::Write for StackText<CAPACITY> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

unsafe fn copy_text(destination: *mut u8, capacity: usize, source: &[u8]) -> isize {
    if source.len() > capacity || (destination.is_null() && !source.is_empty()) {
        return TEXT_OPERATION_FAILED;
    }
    if !source.is_empty() {
        // SAFETY: The caller provides `capacity` writable bytes, and the
        // length check above proves that `source` fits in that region.
        unsafe {
            std::ptr::copy_nonoverlapping(source.as_ptr(), destination, source.len());
        }
    }
    isize::try_from(source.len()).unwrap_or(TEXT_OPERATION_FAILED)
}

unsafe fn format_value(value: impl fmt::Display, destination: *mut u8, capacity: usize) -> isize {
    let mut text = StackText::<FORMAT_CAPACITY>::new();
    if write!(&mut text, "{value}").is_err() {
        return TEXT_OPERATION_FAILED;
    }
    // SAFETY: The caller upholds the destination contract for this ABI call.
    unsafe { copy_text(destination, capacity, text.as_bytes()) }
}

unsafe fn map_case(
    value: u32,
    destination: *mut u8,
    capacity: usize,
    mapping: fn(char, &mut StackText<CASE_MAPPING_CAPACITY>) -> fmt::Result,
) -> isize {
    let Some(character) = char::from_u32(value) else {
        return TEXT_OPERATION_FAILED;
    };
    let mut text = StackText::<CASE_MAPPING_CAPACITY>::new();
    if mapping(character, &mut text).is_err() {
        return TEXT_OPERATION_FAILED;
    }
    // SAFETY: The caller upholds the destination contract for this ABI call.
    unsafe { copy_text(destination, capacity, text.as_bytes()) }
}

fn write_lowercase(
    character: char,
    destination: &mut StackText<CASE_MAPPING_CAPACITY>,
) -> fmt::Result {
    for mapped in character.to_lowercase() {
        destination.write_char(mapped)?;
    }
    Ok(())
}

fn write_uppercase(
    character: char,
    destination: &mut StackText<CASE_MAPPING_CAPACITY>,
) -> fmt::Result {
    for mapped in character.to_uppercase() {
        destination.write_char(mapped)?;
    }
    Ok(())
}

/// Formats one `f32` using Rust's shortest round-trippable display form.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn text_format_f32(
    value: f32,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { format_value(value, destination, capacity) }
}

/// Formats one `f64` using Rust's shortest round-trippable display form.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn text_format_f64(
    value: f64,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { format_value(value, destination, capacity) }
}

/// Formats one signed 128-bit integer in base ten.
///
/// # Safety
///
/// `value` must point to one initialized `i128`. When `capacity` is nonzero,
/// `destination` must point to `capacity` live, writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn text_format_i128(
    value: *const u8,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    if value.is_null() {
        return TEXT_OPERATION_FAILED;
    }
    // SAFETY: The caller guarantees that `value` points to one initialized
    // integer. Unaligned reads keep the ABI independent of source alignment.
    let value = unsafe { value.cast::<i128>().read_unaligned() };
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { format_value(value, destination, capacity) }
}

/// Formats one unsigned 128-bit integer in base ten.
///
/// # Safety
///
/// `value` must point to one initialized `u128`. When `capacity` is nonzero,
/// `destination` must point to `capacity` live, writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn text_format_u128(
    value: *const u8,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    if value.is_null() {
        return TEXT_OPERATION_FAILED;
    }
    // SAFETY: The caller guarantees that `value` points to one initialized
    // integer. Unaligned reads keep the ABI independent of source alignment.
    let value = unsafe { value.cast::<u128>().read_unaligned() };
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { format_value(value, destination, capacity) }
}

/// Encodes one valid Unicode scalar into at most four UTF-8 bytes.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utf8_encode_char(
    value: u32,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    let Some(character) = char::from_u32(value) else {
        return TEXT_OPERATION_FAILED;
    };
    let mut encoded = [0_u8; 4];
    let encoded = character.encode_utf8(&mut encoded);
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { copy_text(destination, capacity, encoded.as_bytes()) }
}

/// Writes the full Unicode lowercase mapping for one scalar.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unicode_lowercase(
    value: u32,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { map_case(value, destination, capacity, write_lowercase) }
}

/// Writes the full Unicode uppercase mapping for one scalar.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unicode_uppercase(
    value: u32,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    // SAFETY: The caller upholds the destination contract documented above.
    unsafe { map_case(value, destination, capacity, write_uppercase) }
}

macro_rules! unicode_property {
    ($name:ident, $operation:ident) => {
        #[must_use]
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(value: u32) -> bool {
            char::from_u32(value).is_some_and(char::$operation)
        }
    };
}

unicode_property!(unicode_is_alphabetic, is_alphabetic);
unicode_property!(unicode_is_alphanumeric, is_alphanumeric);
unicode_property!(unicode_is_numeric, is_numeric);
unicode_property!(unicode_is_whitespace, is_whitespace);
unicode_property!(unicode_is_lowercase, is_lowercase);
unicode_property!(unicode_is_uppercase, is_uppercase);
unicode_property!(unicode_is_control, is_control);

#[cfg(test)]
mod tests {
    use super::{
        text_format_f32, text_format_i128, text_format_u128, unicode_is_alphabetic,
        unicode_is_numeric, unicode_uppercase, utf8_encode_char,
    };

    #[test]
    fn text_helpers_should_format_and_encode_without_allocating() {
        let mut formatted = [0_u8; 64];
        // SAFETY: `formatted` is a live writable output region.
        let formatted_len =
            unsafe { text_format_f32(12.5, formatted.as_mut_ptr(), formatted.len()) };
        assert_eq!(
            &formatted[..usize::try_from(formatted_len).expect("valid length")],
            b"12.5"
        );

        let mut encoded = [0_u8; 4];
        // SAFETY: `encoded` is a live writable output region.
        let encoded_len =
            unsafe { utf8_encode_char('🦀' as u32, encoded.as_mut_ptr(), encoded.len()) };
        assert_eq!(
            &encoded[..usize::try_from(encoded_len).expect("valid length")],
            "🦀".as_bytes()
        );
    }

    #[test]
    fn text_helpers_should_format_full_width_integers() {
        let signed = i128::MIN;
        let unsigned = u128::MAX;
        let mut signed_text = [0_u8; 64];
        let mut unsigned_text = [0_u8; 64];

        // SAFETY: Both integer pointers and output regions remain live.
        let signed_len = unsafe {
            text_format_i128(
                (&raw const signed).cast::<u8>(),
                signed_text.as_mut_ptr(),
                signed_text.len(),
            )
        };
        // SAFETY: Both integer pointers and output regions remain live.
        let unsigned_len = unsafe {
            text_format_u128(
                (&raw const unsigned).cast::<u8>(),
                unsigned_text.as_mut_ptr(),
                unsigned_text.len(),
            )
        };

        assert_eq!(
            &signed_text[..usize::try_from(signed_len).expect("valid length")],
            i128::MIN.to_string().as_bytes()
        );
        assert_eq!(
            &unsigned_text[..usize::try_from(unsigned_len).expect("valid length")],
            u128::MAX.to_string().as_bytes()
        );
    }

    #[test]
    fn unicode_helpers_should_preserve_full_case_mappings_and_properties() {
        let mut mapped = [0_u8; 12];
        // SAFETY: `mapped` is a live writable output region.
        let mapped_len =
            unsafe { unicode_uppercase('ß' as u32, mapped.as_mut_ptr(), mapped.len()) };

        assert_eq!(
            &mapped[..usize::try_from(mapped_len).expect("valid length")],
            b"SS"
        );
        assert!(unicode_is_alphabetic('λ' as u32));
        assert!(unicode_is_numeric('٣' as u32));
    }
}
