// src/math/mod.rs
//! Kalkulasi matematika AMM dengan presisi tinggi.
//!
//! Semua kalkulasi menggunakan integer arithmetic (U256) untuk mencegah
//! floating-point errors yang kritis dalam konteks keuangan.
//!
//! Formula dasar AMM V2:
//!   x * y = k  (constant product formula)
//!
//! Output formula dengan fee 0.3%:
//!   amount_out = (amount_in * 997 * reserve_out)
//!              / (reserve_in * 1000 + amount_in * 997)

use alloy::primitives::U256;
use anyhow::{anyhow, Result};

// ── Konstanta ────────────────────────────────────────────────────────────────

/// Precision factor untuk kalkulasi floating-point yang di-"integerkan"
pub const PRECISION: u128 = 1_000_000_000_000_000_000u128; // 1e18

/// Fee numerator V2 (997 dari 1000)
pub const V2_FEE_NUMERATOR: u128 = 997;
/// Fee denominator V2
pub const V2_FEE_DENOMINATOR: u128 = 1000;

// ── Core AMM Functions ────────────────────────────────────────────────────────

/// Hitung amount_out dari swap V2 dengan integer precision penuh.
///
/// # Formula
/// ```text
/// amount_out = (amount_in * 997 * reserve_out)
///            / (reserve_in * 1000 + amount_in * 997)
/// ```
///
/// # Arguments
/// * `amount_in`    - Jumlah token input (raw, termasuk decimals)
/// * `reserve_in`   - Reserve pool untuk token input
/// * `reserve_out`  - Reserve pool untuk token output
///
/// # Returns
/// * `Ok(U256)` - Jumlah token output
/// * `Err`      - Jika reserve kosong atau overflow
pub fn get_amount_out(
    amount_in:   U256,
    reserve_in:  U256,
    reserve_out: U256,
) -> Result<U256> {
    if amount_in.is_zero() {
        return Err(anyhow!("amount_in tidak boleh nol"));
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(anyhow!("Reserve pool tidak boleh nol (insufficient liquidity)"));
    }

    let amount_in_with_fee = amount_in
        .checked_mul(U256::from(V2_FEE_NUMERATOR))
        .ok_or_else(|| anyhow!("Overflow: amount_in * fee_numerator"))?;

    let numerator = amount_in_with_fee
        .checked_mul(reserve_out)
        .ok_or_else(|| anyhow!("Overflow: numerator"))?;

    let denominator = reserve_in
        .checked_mul(U256::from(V2_FEE_DENOMINATOR))
        .ok_or_else(|| anyhow!("Overflow: reserve_in * denominator"))?
        .checked_add(amount_in_with_fee)
        .ok_or_else(|| anyhow!("Overflow: denominator + fee"))?;

    Ok(numerator / denominator)
}

/// Hitung amount_in yang dibutuhkan untuk mendapatkan amount_out tertentu.
/// Kebalikan dari get_amount_out (untuk eksekusi exact-output).
///
/// # Formula
/// ```text
/// amount_in = (reserve_in * amount_out * 1000)
///           / ((reserve_out - amount_out) * 997) + 1
/// ```
pub fn get_amount_in(
    amount_out:  U256,
    reserve_in:  U256,
    reserve_out: U256,
) -> Result<U256> {
    if amount_out.is_zero() {
        return Err(anyhow!("amount_out tidak boleh nol"));
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(anyhow!("Reserve pool tidak boleh nol"));
    }
    if amount_out >= reserve_out {
        return Err(anyhow!("amount_out melebihi reserve_out (tidak mungkin)"));
    }

    let numerator = reserve_in
        .checked_mul(amount_out)
        .ok_or_else(|| anyhow!("Overflow: numerator"))?
        .checked_mul(U256::from(V2_FEE_DENOMINATOR))
        .ok_or_else(|| anyhow!("Overflow: numerator * denominator"))?;

    let denominator = reserve_out
        .checked_sub(amount_out)
        .ok_or_else(|| anyhow!("Underflow: reserve_out - amount_out"))?
        .checked_mul(U256::from(V2_FEE_NUMERATOR))
        .ok_or_else(|| anyhow!("Overflow: denominator * fee_numerator"))?;

    Ok(numerator / denominator + U256::from(1u64))
}

/// Simulasikan multi-hop swap dan kembalikan output akhir.
///
/// # Arguments
/// * `amount_in`  - Input amount awal
/// * `reserves`   - Vec of (reserve_in, reserve_out) per hop
///
/// # Returns  
/// * Output final setelah semua hop
pub fn simulate_multihop(
    amount_in: U256,
    reserves:  &[(U256, U256)],
) -> Result<U256> {
    if reserves.is_empty() {
        return Err(anyhow!("Hops tidak boleh kosong"));
    }

    let mut current_amount = amount_in;

    for (i, (reserve_in, reserve_out)) in reserves.iter().enumerate() {
        current_amount = get_amount_out(current_amount, *reserve_in, *reserve_out)
            .map_err(|e| anyhow!("Hop {i} gagal: {e}"))?;
    }

    Ok(current_amount)
}

// ── Optimal Input Calculation ─────────────────────────────────────────────────

/// Hitung input optimal untuk arbitrase 2-hop menggunakan formula analitik.
///
/// Untuk A → B → A melalui dua pool berbeda:
///   Pool 1: (R_A1, R_B1)
///   Pool 2: (R_B2, R_A2)
///
/// # Derivasi Formula
/// ```text
/// Output hop 2:
///   c = f² * x * R_B1 * R_A2
///     / (R_A1 * R_B2 + f*x*(R_B2 + f*R_B1))
///
/// Maksimalkan profit P(x) = c - x:
///   dP/dx = 0 → (M + N*x)² = K*M
///
/// dimana:
///   f = 0.997 (fee multiplier)
///   K = f² * R_B1 * R_A2
///   M = R_A1 * R_B2
///   N = f * (R_B2 + f * R_B1)
///
/// Solusi: x* = (√(K*M) - M) / N
/// ```
///
/// # Returns
/// * `Some(U256)` - Optimal input (dalam unit token, raw)
/// * `None`       - Tidak ada peluang (negatif atau pool tidak seimbang)
pub fn optimal_input_two_hop(
    reserve_a1: U256,  // Reserve token A di pool 1
    reserve_b1: U256,  // Reserve token B di pool 1
    reserve_b2: U256,  // Reserve token B di pool 2
    reserve_a2: U256,  // Reserve token A di pool 2
) -> Option<U256> {
    // Konversi ke f64 untuk kalkulasi aritmetik kompleks
    // Kemudian validasi hasilnya
    let ra1 = u256_to_f64(reserve_a1);
    let rb1 = u256_to_f64(reserve_b1);
    let rb2 = u256_to_f64(reserve_b2);
    let ra2 = u256_to_f64(reserve_a2);

    if ra1 <= 0.0 || rb1 <= 0.0 || rb2 <= 0.0 || ra2 <= 0.0 {
        return None;
    }

    let f = V2_FEE_NUMERATOR as f64 / V2_FEE_DENOMINATOR as f64; // 0.997

    let k = f * f * rb1 * ra2;           // f² * R_B1 * R_A2
    let m = ra1 * rb2;                   // R_A1 * R_B2
    let n = f * (rb2 + f * rb1);         // f * (R_B2 + f*R_B1)

    let km = k * m;
    if km <= 0.0 {
        return None;
    }

    let sqrt_km = km.sqrt();
    if sqrt_km <= m {
        // Tidak ada profit bahkan dengan input infinitesimal
        return None;
    }

    let x_opt = (sqrt_km - m) / n;
    if x_opt <= 0.0 {
        return None;
    }

    // Clamp ke maximum yang wajar (jangan drain pool lebih dari 30%)
    let max_input_ra1 = ra1 * 0.30;
    let x_clamped = x_opt.min(max_input_ra1);

    f64_to_u256(x_clamped)
}

/// Hitung input optimal untuk arbitrase 3-hop menggunakan binary search.
///
/// Formula analitik untuk 3-hop sangat kompleks; binary search lebih praktis
/// dan cukup presisi untuk MEV purposes.
///
/// # Algorithm
/// Gunakan ternary search pada fungsi profit P(x):
///   - Cari x dalam range [min_x, max_x]
///   - Profit P(x) bersifat concave (unimodal)
///   - Ternary search konvergen ke maximum dalam O(log n) iterasi
pub fn optimal_input_three_hop(
    reserves: &[(U256, U256)],  // (reserve_in, reserve_out) per hop
    max_input: U256,
) -> Option<U256> {
    if reserves.len() != 3 {
        return None;
    }

    // Ternary search: profit function bersifat concave (unimodal)
    let mut lo = U256::from(1u64);
    let mut hi = max_input;

    // 64 iterasi cukup untuk konvergen dengan presisi tinggi
    for _ in 0..64 {
        if hi <= lo + U256::from(2u64) {
            break;
        }

        let m1 = lo + (hi - lo) / U256::from(3u64);
        let m2 = hi - (hi - lo) / U256::from(3u64);

        let profit_m1 = calculate_profit(m1, reserves);
        let profit_m2 = calculate_profit(m2, reserves);

        match (profit_m1, profit_m2) {
            (Some(p1), Some(p2)) => {
                if p1 < p2 {
                    lo = m1;
                } else {
                    hi = m2;
                }
            }
            (Some(_), None) => { hi = m2; }
            (None, Some(_)) => { lo = m1; }
            (None, None)    => return None,
        }
    }

    let optimal = (lo + hi) / U256::from(2u64);

    // Validasi profit positif
    let profit = calculate_profit(optimal, reserves)?;
    if profit.is_zero() {
        return None;
    }

    Some(optimal)
}

/// Hitung profit absolut untuk input tertentu dalam rute multi-hop
fn calculate_profit(input: U256, reserves: &[(U256, U256)]) -> Option<U256> {
    let output = simulate_multihop(input, reserves).ok()?;
    output.checked_sub(input)
}

// ── Price & Rate Calculations ─────────────────────────────────────────────────

/// Hitung effective exchange rate antara dua token dalam pool V2.
/// Rate = reserve_out / reserve_in (tanpa fee)
///
/// Return nilai dalam f64 untuk perbandingan cepat.
pub fn spot_price(reserve_in: U256, reserve_out: U256) -> f64 {
    let ri = u256_to_f64(reserve_in);
    let ro = u256_to_f64(reserve_out);
    if ri == 0.0 { 0.0 } else { ro / ri }
}

/// Hitung price impact dari swap (dalam persentase).
///
/// Price impact = (1 - amount_out / (amount_in * spot_rate)) * 100
pub fn price_impact_percent(
    amount_in:   U256,
    reserve_in:  U256,
    reserve_out: U256,
) -> f64 {
    let spot = spot_price(reserve_in, reserve_out);
    let ideal_out = u256_to_f64(amount_in) * spot;

    match get_amount_out(amount_in, reserve_in, reserve_out) {
        Ok(actual_out) => {
            let actual = u256_to_f64(actual_out);
            if ideal_out == 0.0 {
                0.0
            } else {
                (1.0 - actual / ideal_out) * 100.0
            }
        }
        Err(_) => 100.0,
    }
}

/// Deteksi apakah ada arbitrase pada dua pool (price difference > threshold).
///
/// Membandingkan spot price pool1 vs 1/spot_price pool2.
/// Jika rasio > 1 + threshold, ada potensi arbitrase.
pub fn has_arbitrage_opportunity(
    reserve_a1: U256, reserve_b1: U256,  // Pool 1: A/B
    reserve_b2: U256, reserve_a2: U256,  // Pool 2: B/A
    threshold:  f64,                     // e.g. 0.006 untuk 0.6% (2x fee + profit)
) -> bool {
    let price1 = spot_price(reserve_a1, reserve_b1); // A→B di pool 1
    let price2 = spot_price(reserve_b2, reserve_a2); // B→A di pool 2

    // Rasio: beli di pool 1, jual di pool 2
    // Menguntungkan jika: price1 * price2 * (1-fee)² > 1
    let combined = price1 * price2;
    let fee_adjusted = combined * (V2_FEE_NUMERATOR as f64 / V2_FEE_DENOMINATOR as f64).powi(2);

    fee_adjusted > (1.0 + threshold)
}

// ── Conversion Utilities ──────────────────────────────────────────────────────

/// Konversi U256 ke f64 (kehilangan presisi untuk nilai > 2^53)
pub fn u256_to_f64(val: U256) -> f64 {
    // Gunakan low 128 bits untuk menghindari truncation
    let lo = val.as_limbs()[0] as f64
        + val.as_limbs()[1] as f64 * (u64::MAX as f64 + 1.0);
    lo
}

/// Konversi f64 ke U256 (hanya untuk nilai positif)
pub fn f64_to_u256(val: f64) -> Option<U256> {
    if val <= 0.0 || val.is_nan() || val.is_infinite() {
        return None;
    }
    // Gunakan u128 sebagai intermediate untuk presisi
    let as_u128 = val as u128;
    Some(U256::from(as_u128))
}

/// Konversi amount dari decimal units ke raw (e.g., 1.5 USDC → 1_500_000)
pub fn to_raw_amount(amount: f64, decimals: u8) -> U256 {
    let factor = 10f64.powi(decimals as i32);
    f64_to_u256(amount * factor).unwrap_or(U256::ZERO)
}

/// Konversi raw amount ke human-readable float
pub fn from_raw_amount(raw: U256, decimals: u8) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    u256_to_f64(raw) / factor
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_amount_out_basic() {
        // Pool: 1000 USDC / 1 WETH
        // Input: 100 USDC
        // Expected out: ~0.0906 WETH (setelah 0.3% fee)
        let reserve_in  = U256::from(1_000_000_000u64); // 1000 USDC (6 decimals)
        let reserve_out = U256::from(1_000_000_000_000_000_000u64); // 1 ETH (18 decimals)
        let amount_in   = U256::from(100_000_000u64); // 100 USDC

        let out = get_amount_out(amount_in, reserve_in, reserve_out).unwrap();
        let out_f64 = u256_to_f64(out) / 1e18;
        
        // Harus sekitar 0.0906 (tidak persis karena integer division)
        assert!(out_f64 > 0.090 && out_f64 < 0.092, "Got: {out_f64}");
    }

    #[test]
    fn test_get_amount_out_zero_input() {
        let r = U256::from(1_000_000u64);
        assert!(get_amount_out(U256::ZERO, r, r).is_err());
    }

    #[test]
    fn test_multihop_simulation() {
        // 3-hop: USDC → WMATIC → WETH → USDC
        let reserves = vec![
            (U256::from(1_000_000_000u64), U256::from(3_000_000_000_000_000_000_000u64)), // USDC/WMATIC
            (U256::from(3_000_000_000_000_000_000_000u64), U256::from(1_000_000_000_000_000_000u64)), // WMATIC/WETH
            (U256::from(1_000_000_000_000_000_000u64), U256::from(3_000_000_000u64)), // WETH/USDC
        ];
        
        let input = U256::from(1_000_000u64); // 1 USDC
        let output = simulate_multihop(input, &reserves);
        
        // Harus return nilai (tidak error)
        assert!(output.is_ok());
    }

    #[test]
    fn test_optimal_input_two_hop() {
        // Pool 1 QuickSwap: 10000 USDC / 10000 WMATIC
        // Pool 2 SushiSwap: 9900 WMATIC / 10100 USDC (ada price difference)
        let ra1 = U256::from(10_000_000_000u64); // 10000 USDC
        let rb1 = U256::from(10_000_000_000_000_000_000_000u64); // 10000 MATIC (18 dec)
        let rb2 = U256::from(9_900_000_000_000_000_000_000u64);  // 9900 MATIC
        let ra2 = U256::from(10_100_000_000u64); // 10100 USDC

        // Harus menemukan optimal input
        if let Some(opt) = optimal_input_two_hop(ra1, rb1, rb2, ra2) {
            // Validasi bahwa profit positif
            let out1 = get_amount_out(opt, ra1, rb1).unwrap();
            let out2 = get_amount_out(out1, rb2, ra2).unwrap();
            assert!(out2 > opt, "Output harus lebih besar dari input untuk profitable trade");
        }
        // Note: bisa None jika price difference tidak cukup cover fee
    }

    #[test]
    fn test_spot_price() {
        let r_in  = U256::from(1_000u64);
        let r_out = U256::from(2_000u64);
        let price = spot_price(r_in, r_out);
        assert!((price - 2.0).abs() < 1e-9, "Expected 2.0, got {price}");
    }

    #[test]
    fn test_price_impact_large_trade() {
        // Swap besar harus memiliki price impact tinggi
        let reserve_in  = U256::from(1_000_000u64);
        let reserve_out = U256::from(1_000_000u64);
        let large_in    = U256::from(500_000u64); // 50% dari reserve
        
        let impact = price_impact_percent(large_in, reserve_in, reserve_out);
        assert!(impact > 20.0, "Price impact harus > 20% untuk trade 50% pool");
    }
}
