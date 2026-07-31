//! Native helpers for allocator-backed hash collections.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::mem::size_of;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub const CONTROL_GROUP_MASKS_SYMBOL: &str = "control_group_masks";
pub const HASH_BYTES_SYMBOL: &str = "hash_bytes";
pub const HASH_SEED_SYMBOL: &str = "hash_seed";

const GROUP_WIDTH: usize = 16;
const EMPTY: u8 = 0x80;
const DELETED: u8 = 0xfe;

static HASH_RANDOM_STATE: OnceLock<RandomState> = OnceLock::new();
static HASH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Creates a process-randomized seed for one hash collection.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn hash_seed() -> u64 {
    let sequence = HASH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut hasher = HASH_RANDOM_STATE
        .get_or_init(RandomState::new)
        .build_hasher();
    hasher.write_u64(sequence);
    hasher.finish()
}

/// Hashes a bounded byte sequence using the same structural mixer as generated code.
///
/// # Safety
///
/// `data` must be null with a zero `length`, or point to `length` readable bytes.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hash_bytes(data: *const u8, length: usize, seed: u64) -> u64 {
    let bytes = if length == 0 {
        &[]
    } else if data.is_null() {
        return mix_word(seed, u64::MAX);
    } else {
        // SAFETY: The ABI contract requires a readable region of exactly `length` bytes.
        unsafe { std::slice::from_raw_parts(data, length) }
    };
    let mut hash = mix_word(seed, length as u64);
    let mut chunks = bytes.chunks_exact(size_of::<u64>());
    for chunk in &mut chunks {
        let mut word = [0_u8; size_of::<u64>()];
        word.copy_from_slice(chunk);
        let word = u64::from_le_bytes(word);
        hash = mix_word(hash, word);
    }
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let mut tail = [0_u8; size_of::<u64>()];
        tail[..remainder.len()].copy_from_slice(remainder);
        hash = mix_word(hash, u64::from_le_bytes(tail));
    }
    hash
}

/// Compares one wrapping group of control bytes with an `H2` fingerprint.
///
/// Bits `0..16` report matching fingerprints, bits `16..32` report empty
/// controls, and bits `32..48` report deleted controls.
///
/// # Safety
///
/// `controls` must point to `capacity` readable bytes. `capacity` must be a
/// power of two no smaller than 16, and `start` must be below `capacity`.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn control_group_masks(
    controls: *const u8,
    capacity: usize,
    start: usize,
    fingerprint: u8,
) -> u64 {
    if controls.is_null()
        || capacity < GROUP_WIDTH
        || !capacity.is_power_of_two()
        || start >= capacity
    {
        return 0;
    }

    let mut wrapped = [0_u8; GROUP_WIDTH];
    let group = if start <= capacity - GROUP_WIDTH {
        // SAFETY: The validated contiguous group remains inside the control allocation.
        unsafe { std::slice::from_raw_parts(controls.add(start), GROUP_WIDTH) }
    } else {
        for (offset, control) in wrapped.iter_mut().enumerate() {
            let index = (start + offset) & (capacity - 1);
            // SAFETY: The power-of-two mask keeps every index below `capacity`.
            *control = unsafe { *controls.add(index) };
        }
        &wrapped
    };
    let (matches, empty, deleted) = group_masks(group, fingerprint);
    u64::from(matches) | (u64::from(empty) << 16) | (u64::from(deleted) << 32)
}

fn mix_word(seed: u64, word: u64) -> u64 {
    let mut mixed = seed ^ word.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

#[cfg(target_arch = "x86_64")]
fn group_masks(group: &[u8], fingerprint: u8) -> (u16, u16, u16) {
    // SAFETY: x86-64 guarantees SSE2, and `group` contains exactly 16 readable bytes.
    unsafe { x86_64_group_masks(group.as_ptr(), fingerprint) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "SSE2's explicit unaligned load accepts byte-aligned control groups"
)]
unsafe fn x86_64_group_masks(group: *const u8, fingerprint: u8) -> (u16, u16, u16) {
    use std::arch::x86_64::{
        __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
    };

    // SAFETY: The caller guarantees 16 readable bytes; unaligned loads are supported.
    let controls = unsafe { _mm_loadu_si128(group.cast::<__m128i>()) };
    let fingerprint = _mm_set1_epi8(fingerprint.cast_signed());
    let empty = _mm_set1_epi8(EMPTY.cast_signed());
    let deleted = _mm_set1_epi8(DELETED.cast_signed());
    (
        u16::try_from(_mm_movemask_epi8(_mm_cmpeq_epi8(controls, fingerprint)))
            .expect("an SSE2 byte mask always contains exactly sixteen bits"),
        u16::try_from(_mm_movemask_epi8(_mm_cmpeq_epi8(controls, empty)))
            .expect("an SSE2 byte mask always contains exactly sixteen bits"),
        u16::try_from(_mm_movemask_epi8(_mm_cmpeq_epi8(controls, deleted)))
            .expect("an SSE2 byte mask always contains exactly sixteen bits"),
    )
}

#[cfg(target_arch = "aarch64")]
fn group_masks(group: &[u8], fingerprint: u8) -> (u16, u16, u16) {
    // SAFETY: AArch64 guarantees NEON, and `group` contains exactly 16 readable bytes.
    unsafe { aarch64_group_masks(group.as_ptr(), fingerprint) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn aarch64_group_masks(group: *const u8, fingerprint: u8) -> (u16, u16, u16) {
    use std::arch::aarch64::{vceqq_u8, vdupq_n_u8, vld1q_u8};

    // SAFETY: The caller guarantees 16 readable bytes.
    let controls = unsafe { vld1q_u8(group) };
    (
        neon_mask(vceqq_u8(controls, vdupq_n_u8(fingerprint))),
        neon_mask(vceqq_u8(controls, vdupq_n_u8(EMPTY))),
        neon_mask(vceqq_u8(controls, vdupq_n_u8(DELETED))),
    )
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon_mask(lanes: std::arch::aarch64::uint8x16_t) -> u16 {
    use std::arch::aarch64::{vaddv_u8, vandq_u8, vget_high_u8, vget_low_u8, vld1q_u8};

    let weights = [1_u8, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: `weights` contains exactly 16 readable bytes.
    let weights = unsafe { vld1q_u8(weights.as_ptr()) };
    let weighted = vandq_u8(lanes, weights);
    u16::from(vaddv_u8(vget_low_u8(weighted))) | (u16::from(vaddv_u8(vget_high_u8(weighted))) << 8)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn group_masks(group: &[u8], fingerprint: u8) -> (u16, u16, u16) {
    let mut matches = 0_u16;
    let mut empty = 0_u16;
    let mut deleted = 0_u16;
    for (index, control) in group.iter().copied().enumerate() {
        let bit = 1_u16 << index;
        matches |= if control == fingerprint { bit } else { 0 };
        empty |= if control == EMPTY { bit } else { 0 };
        deleted |= if control == DELETED { bit } else { 0 };
    }
    (matches, empty, deleted)
}

#[cfg(test)]
mod tests {
    use super::{DELETED, EMPTY, control_group_masks, hash_bytes, hash_seed};

    fn packed_lane_mask(masks: u64, shift: u32) -> u16 {
        let selected = (masks >> shift) & u64::from(u16::MAX);
        u16::try_from(selected).expect("the packed lane mask is explicitly limited to sixteen bits")
    }

    #[test]
    fn hash_seed_should_change_between_collections() {
        assert_ne!(hash_seed(), hash_seed());
    }

    #[test]
    fn byte_hash_should_be_repeatable_for_one_seed() {
        let bytes = b"stable hash input";

        // SAFETY: `bytes` remains readable for its exact length.
        let left = unsafe { hash_bytes(bytes.as_ptr(), bytes.len(), 42) };
        // SAFETY: `bytes` remains readable for its exact length.
        let right = unsafe { hash_bytes(bytes.as_ptr(), bytes.len(), 42) };

        assert_eq!(left, right);
    }

    #[test]
    fn control_group_should_report_wrapping_matches_and_states() {
        let mut controls = [EMPTY; 32];
        controls[30] = 7;
        controls[31] = DELETED;
        controls[0] = 7;

        // SAFETY: The array is live, power-of-two sized, and `start` is in range.
        let masks = unsafe { control_group_masks(controls.as_ptr(), controls.len(), 30, 7) };

        assert_eq!(packed_lane_mask(masks, 0), 0b0101);
        assert_eq!(packed_lane_mask(masks, 16), 0xfff8);
        assert_eq!(packed_lane_mask(masks, 32), 0b0010);
    }
}
