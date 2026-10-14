// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Hotel8:Upsell integration tests (test-strategy Layer 3) — DB-free.
//!
//! Exercises the extracted state machine (`offer_state`) + pricing rule
//! (`pricing`) exactly as the handlers apply them.

use hotel8_upsell_server::offer_state as states;
use hotel8_upsell_server::pricing;

use states::TransactionState::*;

#[test]
fn offer_lifecycle_happy_path() {
    let mut st = Offered;
    assert!(st.can_transition_to(Accepted));
    st = Accepted;
    assert!(states::TransactionState::from_label(st.label()) == Accepted);
    // Terminal — no further transitions.
    assert!(!st.can_transition_to(Declined));
    assert!(!st.can_transition_to(Expired));
}

#[test]
fn offer_decline_and_lapse() {
    assert!(Offered.can_transition_to(Declined));
    assert!(Offered.can_transition_to(Expired));
    assert!(states::is_expired(Offered, 24.0));
    assert!(!states::is_expired(Offered, 23.99));
}

#[test]
fn decided_offers_never_lapse() {
    // The 24h expiry sweep must not touch decided transactions.
    assert!(!states::is_expired(Accepted, 100.0));
    assert!(!states::is_expired(Declined, 48.0));
}

#[test]
fn pricing_matches_handler_defaults() {
    // Handler defaults: base 100.0, occupancy 0.5, 7 days out, tier NONE.
    let p = pricing::compute(100.0, 0.5, 7, "NONE");
    assert_eq!(p, 115.0);
}

#[test]
fn pricing_urgency_is_monotonic() {
    let mut prev = pricing::compute(100.0, 0.5, 90, "NONE");
    for d in [60, 30, 14, 7, 3, 1, 0] {
        let p = pricing::compute(100.0, 0.5, d, "NONE");
        assert!(p >= prev, "price decreased approaching arrival at {d} days");
        prev = p;
    }
}

#[test]
fn pricing_tier_discounts_order() {
    let none = pricing::compute(100.0, 0.5, 7, "NONE");
    let gold = pricing::compute(100.0, 0.5, 7, "GOLD");
    let platinum = pricing::compute(100.0, 0.5, 7, "PLATINUM");
    let diamond = pricing::compute(100.0, 0.5, 7, "DIAMOND");
    assert!(gold < none && platinum < gold && diamond < platinum);
}

#[test]
fn acceptance_rate_matches_analytics_semantics() {
    // get_analytics: decided = accepted + declined; expireds excluded.
    assert_eq!(states::acceptance_rate_pct(7, 3, 10), Some(70.0));
    assert_eq!(states::acceptance_rate_pct(0, 0, 4), None); // nothing decided
}

#[test]
fn state_labels_round_trip() {
    for t in [Offered, Accepted, Declined, Expired] {
        assert_eq!(states::TransactionState::from_label(t.label()), t);
    }
}
