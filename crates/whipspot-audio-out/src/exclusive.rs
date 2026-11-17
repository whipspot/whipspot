//! Platform-neutral parts of exclusive output (MD42): the format and rate negotiation order,
//! the HRESULT mapping and the packing of a rendered buffer into device bytes. Pure functions,
//! unit-tested on any host; the WASAPI adapter (Windows) calls them.

use whipspot_core::engine::{DeviceFormat, OutputError, SampleFormat};

/// The standard device rates, ascending (44.1 to 384 kHz).
pub const STANDARD_RATES: [u32; 8] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// Lowest and highest rate a request may name (the engine's own bounds).
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 384_000;

/// One device format to probe: what the renderer produces and the device container width.
///
/// `container_bits` is 32, 24 (packed) or 16; packed 24 is rendered as `I32`/24 and packed
/// by [`pack`]. `format.channels` is always 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub format: DeviceFormat,
    pub container_bits: u8,
}

/// The candidates at `rate` in MD42 order: 32-bit int, 24-in-32, packed 24, 16-bit, 32-bit
/// float. A non-empty `preference` keeps only those sample types, ordered by it (the `I32`
/// variants keep their relative order).
pub fn candidates(rate: u32, preference: &[SampleFormat]) -> Vec<Candidate> {
    let c = |sample, valid_bits, container_bits| Candidate {
        format: DeviceFormat {
            rate,
            channels: 2,
            sample,
            valid_bits,
        },
        container_bits,
    };
    let all = [
        c(SampleFormat::I32, 32, 32),
        c(SampleFormat::I32, 24, 32),
        c(SampleFormat::I32, 24, 24),
        c(SampleFormat::I16, 16, 16),
        c(SampleFormat::F32, 32, 32),
    ];
    if preference.is_empty() {
        return all.to_vec();
    }
    let mut out = Vec::with_capacity(all.len());
    for (i, sample) in preference.iter().enumerate() {
        if preference[..i].contains(sample) {
            continue;
        }
        out.extend(all.iter().filter(|c| c.format.sample == *sample));
    }
    out
}

/// The rates to try for `wanted`, in MD42 order: `wanted`, then the standard rates that are
/// integer multiples of it ascending, then the other standard rates above it ascending, then
/// those below it descending. A `wanted` outside 8 to 384 kHz gives the standard rates
/// ascending.
pub fn rate_order(wanted: u32) -> Vec<u32> {
    if !(MIN_RATE..=MAX_RATE).contains(&wanted) {
        return STANDARD_RATES.to_vec();
    }
    let mut out = vec![wanted];
    for &rate in STANDARD_RATES
        .iter()
        .filter(|&&r| r > wanted && r % wanted == 0)
    {
        out.push(rate);
    }
    for &rate in STANDARD_RATES.iter().filter(|&&r| r > wanted) {
        if !out.contains(&rate) {
            out.push(rate);
        }
    }
    for &rate in STANDARD_RATES.iter().rev().filter(|&&r| r < wanted) {
        if !out.contains(&rate) {
            out.push(rate);
        }
    }
    out
}

/// The first rate in [`rate_order`] with any supported candidate, and the first supported
/// candidate there; `None` when nothing is supported.
pub fn negotiate(
    wanted: u32,
    preference: &[SampleFormat],
    mut supported: impl FnMut(&Candidate) -> bool,
) -> Option<Candidate> {
    rate_order(wanted).into_iter().find_map(|rate| {
        candidates(rate, preference)
            .into_iter()
            .find(|c| supported(c))
    })
}

/// AUDCLNT_E_DEVICE_INVALIDATED.
pub const DEVICE_INVALIDATED: u32 = 0x8889_0004;
/// AUDCLNT_E_UNSUPPORTED_FORMAT.
pub const UNSUPPORTED_FORMAT: u32 = 0x8889_0008;
/// AUDCLNT_E_DEVICE_IN_USE.
pub const DEVICE_IN_USE: u32 = 0x8889_000A;
/// AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED.
pub const EXCLUSIVE_MODE_NOT_ALLOWED: u32 = 0x8889_000E;
/// AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED.
pub const BUFFER_SIZE_NOT_ALIGNED: u32 = 0x8889_0019;

/// An audio-client HRESULT as an [`OutputError`].
pub fn map_hresult(code: u32) -> OutputError {
    match code {
        DEVICE_IN_USE => OutputError::Busy,
        UNSUPPORTED_FORMAT | EXCLUSIVE_MODE_NOT_ALLOWED => OutputError::Unsupported,
        DEVICE_INVALIDATED => OutputError::NoDevice,
        _ => OutputError::Failed,
    }
}

/// A rendered buffer to pack (the sample type the renderer filled).
#[derive(Clone, Copy, Debug)]
pub enum OutSamples<'a> {
    F32(&'a [f32]),
    I16(&'a [i16]),
    I32(&'a [i32]),
}

/// Write `src` into `out` as little-endian device bytes: F32 4 bytes, I16 2, I32 4 with
/// `container_bits` 32, or bytes 1..4 of the left-justified i32 with `container_bits` 24.
/// Packs as many samples as both sides hold; allocates nothing.
pub fn pack(out: &mut [u8], container_bits: u8, src: OutSamples<'_>) {
    match src {
        OutSamples::F32(s) => {
            for (d, v) in out.chunks_exact_mut(4).zip(s) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        OutSamples::I16(s) => {
            for (d, v) in out.chunks_exact_mut(2).zip(s) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        OutSamples::I32(s) if container_bits == 24 => {
            for (d, v) in out.chunks_exact_mut(3).zip(s) {
                d.copy_from_slice(&v.to_le_bytes()[1..4]);
            }
        }
        OutSamples::I32(s) => {
            for (d, v) in out.chunks_exact_mut(4).zip(s) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(rate: u32, sample: SampleFormat, valid_bits: u8, container_bits: u8) -> Candidate {
        Candidate {
            format: DeviceFormat {
                rate,
                channels: 2,
                sample,
                valid_bits,
            },
            container_bits,
        }
    }

    fn key(c: &Candidate) -> (u32, SampleFormat, u8, u8) {
        (
            c.format.rate,
            c.format.sample,
            c.format.valid_bits,
            c.container_bits,
        )
    }

    #[test]
    fn candidate_order() {
        let all = candidates(48_000, &[]);
        let keys: Vec<_> = all.iter().map(key).collect();
        assert_eq!(
            keys,
            vec![
                (48_000, SampleFormat::I32, 32, 32),
                (48_000, SampleFormat::I32, 24, 32),
                (48_000, SampleFormat::I32, 24, 24),
                (48_000, SampleFormat::I16, 16, 16),
                (48_000, SampleFormat::F32, 32, 32),
            ]
        );
        assert!(all.iter().all(|c| c.format.channels == 2));
        let pref = candidates(48_000, &[SampleFormat::I16, SampleFormat::I32]);
        let keys: Vec<_> = pref.iter().map(key).collect();
        assert_eq!(
            keys,
            vec![
                (48_000, SampleFormat::I16, 16, 16),
                (48_000, SampleFormat::I32, 32, 32),
                (48_000, SampleFormat::I32, 24, 32),
                (48_000, SampleFormat::I32, 24, 24),
            ]
        );
    }

    #[test]
    fn n1_best_format_at_the_wanted_rate() {
        let table = [
            cand(44_100, SampleFormat::I32, 24, 32),
            cand(44_100, SampleFormat::I16, 16, 16),
        ];
        let got = negotiate(44_100, &[], |c| table.contains(c));
        assert_eq!(got, Some(cand(44_100, SampleFormat::I32, 24, 32)));
    }

    #[test]
    fn n2_next_standard_rate_above() {
        let got = negotiate(44_100, &[], |c| matches!(c.format.rate, 48_000 | 96_000));
        assert_eq!(got, Some(cand(48_000, SampleFormat::I32, 32, 32)));
    }

    #[test]
    fn n3_a_multiple_before_another_family() {
        let got = negotiate(44_100, &[], |c| matches!(c.format.rate, 88_200 | 48_000));
        assert_eq!(got.map(|c| c.format.rate), Some(88_200));
    }

    #[test]
    fn n4_preference_filters_sample_types() {
        let only = candidates(44_100, &[SampleFormat::F32]);
        assert_eq!(only, vec![cand(44_100, SampleFormat::F32, 32, 32)]);
        let got = negotiate(44_100, &[SampleFormat::F32], |_| true);
        assert_eq!(got, Some(cand(44_100, SampleFormat::F32, 32, 32)));
        let mut probed = Vec::new();
        let _ = negotiate(44_100, &[SampleFormat::F32], |c| {
            probed.push(c.format.sample);
            false
        });
        assert!(probed.iter().all(|s| *s == SampleFormat::F32));
    }

    #[test]
    fn n5_nothing_supported() {
        assert_eq!(negotiate(44_100, &[], |_| false), None);
    }

    #[test]
    fn n6_packed_24_only() {
        let got = negotiate(96_000, &[], |c| c.container_bits == 24);
        assert_eq!(got, Some(cand(96_000, SampleFormat::I32, 24, 24)));
    }

    #[test]
    fn rate_order_44k1() {
        assert_eq!(
            rate_order(44_100),
            vec![
                44_100, 88_200, 176_400, 352_800, 48_000, 96_000, 192_000, 384_000
            ]
        );
    }

    #[test]
    fn rate_order_32k() {
        // Only the standard rates that are multiples of 32 kHz come first.
        assert_eq!(
            rate_order(32_000),
            vec![
                32_000, 96_000, 192_000, 384_000, 44_100, 48_000, 88_200, 176_400, 352_800
            ]
        );
    }

    #[test]
    fn rate_order_edges() {
        assert_eq!(
            rate_order(96_000),
            vec![
                96_000, 192_000, 384_000, 176_400, 352_800, 88_200, 48_000, 44_100
            ]
        );
        assert_eq!(rate_order(0), STANDARD_RATES.to_vec());
        assert_eq!(rate_order(400_000), STANDARD_RATES.to_vec());
        assert_eq!(rate_order(384_000)[0], 384_000);
        let order = rate_order(8_000);
        let mut dedup = order.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), order.len());
    }

    #[test]
    fn hresult_mapping() {
        assert_eq!(map_hresult(0x8889_000A), OutputError::Busy);
        assert_eq!(map_hresult(0x8889_0008), OutputError::Unsupported);
        assert_eq!(map_hresult(0x8889_000E), OutputError::Unsupported);
        assert_eq!(map_hresult(0x8889_0004), OutputError::NoDevice);
        assert_eq!(map_hresult(0x8889_0019), OutputError::Failed);
        assert_eq!(map_hresult(0x8000_4005), OutputError::Failed);
        assert_eq!(map_hresult(0), OutputError::Failed);
    }

    #[test]
    fn pack_f32() {
        let mut out = [0u8; 8];
        pack(&mut out, 32, OutSamples::F32(&[1.0, -1.0]));
        assert_eq!(out[..4], 1.0f32.to_le_bytes());
        assert_eq!(out[4..], (-1.0f32).to_le_bytes());
    }

    #[test]
    fn pack_i16() {
        let mut out = [0u8; 8];
        pack(
            &mut out,
            16,
            OutSamples::I16(&[-1, i16::MIN, i16::MAX, 0x0102]),
        );
        assert_eq!(out, [0xFF, 0xFF, 0x00, 0x80, 0xFF, 0x7F, 0x02, 0x01]);
    }

    #[test]
    fn pack_i32_container_32() {
        let mut out = [0u8; 12];
        pack(&mut out, 32, OutSamples::I32(&[-1, i32::MIN, i32::MAX]));
        assert_eq!(
            out,
            [
                0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0x80, 0xFF, 0xFF, 0xFF, 0x7F
            ]
        );
    }

    #[test]
    fn pack_i32_packed_24() {
        // Left-justified 24-bit values: the device takes the top three bytes.
        let mut out = [0u8; 12];
        let src = [-1 << 8, i32::MIN, i32::MAX & !0xFF, 0x0102_0300];
        pack(&mut out, 24, OutSamples::I32(&src));
        assert_eq!(
            out,
            [
                0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F, 0x03, 0x02, 0x01
            ]
        );
        // i32::MAX and -1 keep their top three bytes (the low byte is dropped).
        let mut out = [0u8; 6];
        pack(&mut out, 24, OutSamples::I32(&[i32::MAX, -1]));
        assert_eq!(out, [0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn pack_stops_at_the_shorter_side() {
        let mut out = [0xAAu8; 6];
        pack(&mut out, 16, OutSamples::I16(&[1, 2]));
        assert_eq!(out, [1, 0, 2, 0, 0xAA, 0xAA]);
    }
}
