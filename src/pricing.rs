// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Pure dynamic-pricing rule - DB-free, testable.
//!
//! The local fallback rule mirrors the historical inline math; the
//! authoritative price comes from a Sentin8:Func WASM module when deployed
//! (`func_exec::run_pricing_module`). Higher occupancy and closer arrival
//! raise the price; VIP tiers get a fixed discount.

/// Compute the suggested price for an upsell offer.
pub fn compute(
    base_price: f64,
    occupancy_rate: f64,
    days_before_arrival: i64,
    guest_tier: &str,
) -> f64 {
    let mut dynamic_price = base_price;
    // Higher occupancy → higher price.
    dynamic_price *= 1.0 + (occupancy_rate * 0.3);
    // Closer to arrival → higher price.
    let urgency_factor = 1.0 + (7.0 - days_before_arrival as f64).max(0.0) * 0.05;
    dynamic_price *= urgency_factor;
    // VIP guests get a discount.
    let tier_discount = match guest_tier {
        "GOLD" => 0.05,
        "PLATINUM" => 0.10,
        "DIAMOND" => 0.15,
        _ => 0.0,
    };
    dynamic_price *= 1.0 - tier_discount;
    (dynamic_price * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_matches_legacy_math() {
        let p = compute(100.0, 0.5, 7, "NONE");
        assert_eq!(p, 115.0); // 100 * 1.15 (occupancy) * 1.0 (urgency)
    }

    #[test]
    fn closer_arrival_raises_price() {
        let far = compute(100.0, 0.5, 30, "NONE");
        let near = compute(100.0, 0.5, 0, "NONE");
        assert!(near > far);
    }

    #[test]
    fn diamond_tier_discounts() {
        let none = compute(100.0, 0.5, 7, "NONE");
        let diamond = compute(100.0, 0.5, 7, "DIAMOND");
        assert!(diamond < none);
    }

    #[test]
    fn negative_days_clamped() {
        // Days-before-arrival below 0 must not produce a negative factor.
        let p = compute(100.0, 0.5, -5, "NONE");
        assert!(p > 0.0);
    }
}
