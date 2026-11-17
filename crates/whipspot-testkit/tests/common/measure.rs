//! Measurement instruments for the audio tests (M10 step 7, H3/H4). Pure functions on `f64`
//! slices; none of them is trusted until `audio_measure.rs` has checked it on a known input
//! (an instrument that fails quietly gives a plausible number, not an error).

use std::f64::consts::PI;

/// SplitMix64: a small seeded generator for noise inputs (no dependency, deterministic).
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The periodic Hann window of `n` points.
pub fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos())
        .collect()
}

/// In-place radix-2 FFT of `re + j im` (length a power of two).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

pub fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// Peak amplitude of the component at `freq` Hz (sample rate `fs`): the Hann-windowed DFT
/// evaluated exactly at `freq`, normalised so a sine of amplitude A reads A. Windowing keeps
/// the main tone's leakage far below the level of a distant component being looked for.
pub fn tone_amp(x: &[f64], freq: f64, fs: f64) -> f64 {
    let w = hann(x.len());
    let step = 2.0 * PI * freq / fs;
    let (mut c, mut s, mut sum) = (0.0, 0.0, 0.0);
    for (n, (&v, &wn)) in x.iter().zip(&w).enumerate() {
        let (sn, cs) = (step * n as f64).sin_cos();
        c += v * wn * cs;
        s += v * wn * sn;
        sum += wn;
    }
    2.0 * (c * c + s * s).sqrt() / sum
}

/// Least-squares fit of `x[n] ≈ Σ a_k cos(w_k n) + b_k sin(w_k n) + dc` at the known
/// frequencies. Returns the amplitude of each frequency and the RMS of the residual
/// (everything that is not those tones or DC: distortion, noise, intermodulation).
pub fn fit_tones(x: &[f64], freqs: &[f64], fs: f64) -> (Vec<f64>, f64) {
    let m = 2 * freqs.len() + 1;
    let basis = |n: usize| -> Vec<f64> {
        let mut b = Vec::with_capacity(m);
        for &f in freqs {
            let (s, c) = (2.0 * PI * f / fs * n as f64).sin_cos();
            b.push(c);
            b.push(s);
        }
        b.push(1.0);
        b
    };
    let mut a = vec![vec![0.0; m + 1]; m];
    for (n, &v) in x.iter().enumerate() {
        let b = basis(n);
        for i in 0..m {
            for j in 0..m {
                a[i][j] += b[i] * b[j];
            }
            a[i][m] += b[i] * v;
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..m {
        let piv = (col..m)
            .max_by(|&p, &q| a[p][col].abs().total_cmp(&a[q][col].abs()))
            .unwrap();
        a.swap(col, piv);
        let (done, rest) = a.split_at_mut(col + 1);
        let pivot_row = &done[col];
        for row in rest {
            let k = row[col] / pivot_row[col];
            for (r, p) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                *r -= k * p;
            }
        }
    }
    let mut coef = vec![0.0; m];
    for i in (0..m).rev() {
        let tail: f64 = (i + 1..m).map(|j| a[i][j] * coef[j]).sum();
        coef[i] = (a[i][m] - tail) / a[i][i];
    }
    let mut res = 0.0;
    for (n, &v) in x.iter().enumerate() {
        let fit: f64 = basis(n).iter().zip(&coef).map(|(b, c)| b * c).sum();
        res += (v - fit) * (v - fit);
    }
    let amps = (0..freqs.len())
        .map(|k| coef[2 * k].hypot(coef[2 * k + 1]))
        .collect();
    (amps, (res / x.len() as f64).sqrt())
}

/// THD+N in dB: the fit residual of a sine at `freq` over the fundamental's RMS.
pub fn thd_n_db(x: &[f64], freq: f64, fs: f64) -> f64 {
    let (amp, resid) = fit_tones(x, &[freq], fs);
    db(resid / (amp[0] / 2f64.sqrt()))
}

/// Welch power spectrum: the mean of Hann-windowed periodograms (`nfft` points, 50 % overlap),
/// bins 0..=nfft/2, normalised so white noise of variance σ² reads σ² in every bin.
pub fn welch(x: &[f64], nfft: usize) -> Vec<f64> {
    let w = hann(nfft);
    let norm: f64 = w.iter().map(|v| v * v).sum();
    let mut acc = vec![0.0; nfft / 2 + 1];
    let mut count = 0usize;
    let mut start = 0;
    while start + nfft <= x.len() {
        let mut re: Vec<f64> = x[start..start + nfft]
            .iter()
            .zip(&w)
            .map(|(a, b)| a * b)
            .collect();
        let mut im = vec![0.0; nfft];
        fft(&mut re, &mut im);
        for (k, p) in acc.iter_mut().enumerate() {
            *p += re[k] * re[k] + im[k] * im[k];
        }
        count += 1;
        start += nfft / 2;
    }
    assert!(count > 0, "signal shorter than one FFT");
    acc.iter().map(|p| p / (norm * count as f64)).collect()
}

/// Mean power (dB re 1) of the Welch bins inside each 1/3-octave band (centres
/// `1000 · 10^(k/10)` Hz) between `lo` and `hi` Hz that holds at least `min_bins` bins.
/// Returns `(centre Hz, dB)`.
pub fn third_octave_db(
    psd: &[f64],
    nfft: usize,
    fs: f64,
    lo: f64,
    hi: f64,
    min_bins: usize,
) -> Vec<(f64, f64)> {
    let bin = fs / nfft as f64;
    let mut out = Vec::new();
    for k in -20..=13 {
        let fc = 1000.0 * 10f64.powf(f64::from(k) / 10.0);
        let (a, b) = (fc * 10f64.powf(-0.05), fc * 10f64.powf(0.05));
        if a < lo || b > hi {
            continue;
        }
        let bins: Vec<f64> = (0..psd.len())
            .filter(|&i| (a..b).contains(&(i as f64 * bin)))
            .map(|i| psd[i])
            .collect();
        if bins.len() >= min_bins {
            out.push((
                fc,
                10.0 * (bins.iter().sum::<f64>() / bins.len() as f64).log10(),
            ));
        }
    }
    out
}

/// The largest spectral line outside `tone_hz +- exclude_hz` and above 30 Hz, in dB relative
/// to `amp` (the tone's amplitude): a Hann-windowed FFT of the middle `nfft` points of `x`.
/// Images, aliases, harmonics and intermodulation all show up as lines. Returns
/// `(dB, Hz)` of the worst one.
pub fn worst_spur_db(
    x: &[f64],
    tone_hz: f64,
    amp: f64,
    exclude_hz: f64,
    fs: f64,
    nfft: usize,
) -> (f64, f64) {
    assert!(x.len() >= nfft);
    let from = (x.len() - nfft) / 2;
    let w = hann(nfft);
    let sum: f64 = w.iter().sum();
    let mut re: Vec<f64> = x[from..from + nfft]
        .iter()
        .zip(&w)
        .map(|(a, b)| a * b)
        .collect();
    let mut im = vec![0.0; nfft];
    fft(&mut re, &mut im);
    let bin = fs / nfft as f64;
    let mut worst = (f64::MIN, 0.0);
    for k in 1..nfft / 2 {
        let f = k as f64 * bin;
        if f < 30.0 || (f - tone_hz).abs() < exclude_hz {
            continue;
        }
        let d = db(2.0 * re[k].hypot(im[k]) / sum / amp);
        if d > worst.0 {
            worst = (d, f);
        }
    }
    worst
}
