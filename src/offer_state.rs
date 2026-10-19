// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Pure upsell offer-transaction state machine - DB-free.
//!
//! `TransactionState` over the transaction `status` TEXT values
//! (OFFERED/ACCEPTED/DECLINED/EXPIRED - see `services/upsell/sql/init.sql`).
//! The SQL guards (`AND status = 'OFFERED'`) mirror `can_transition_to`.

/// Upsell transaction states (TEXT literals in `upsell_upsell_transactions.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Offered,
    Accepted,
    Declined,
    Expired,
}

impl TransactionState {
    pub fn label(self) -> &'static str {
        match self {
            TransactionState::Offered => "OFFERED",
            TransactionState::Accepted => "ACCEPTED",
            TransactionState::Declined => "DECLINED",
            TransactionState::Expired => "EXPIRED",
        }
    }

    /// Parse a DB TEXT status (unknown → OFFERED).
    pub fn from_label(s: &str) -> Self {
        match s {
            "ACCEPTED" => TransactionState::Accepted,
            "DECLINED" => TransactionState::Declined,
            "EXPIRED" => TransactionState::Expired,
            _ => TransactionState::Offered,
        }
    }

    /// Allowed transitions: accept/decline only from OFFERED; OFFERED can
    /// lapse to EXPIRED. Terminal states are dead ends.
    pub fn can_transition_to(self, to: TransactionState) -> bool {
        use TransactionState::*;
        match (self, to) {
            (Offered, Accepted) | (Offered, Declined) | (Offered, Expired) => true,
            _ => false,
        }
    }
}

/// Expiry rule: an OFFERED transaction older than 24h is expired.
pub fn is_expired(status: TransactionState, created_at_hours_ago: f64) -> bool {
    status == TransactionState::Offered && created_at_hours_ago >= 24.0
}

/// Analytics: acceptance rate as a percentage of decided transactions.
/// Returns `None` when nothing was decided yet (avoids 0/0).
pub fn acceptance_rate_pct(accepted: usize, declined: usize, _expired: usize) -> Option<f64> {
    let decided = accepted + declined;
    if decided == 0 {
        return None;
    }
    Some((accepted as f64 / decided as f64) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use TransactionState::*;

    #[test]
    fn labels_match_db_literals() {
        assert_eq!(Offered.label(), "OFFERED");
        assert_eq!(Accepted.label(), "ACCEPTED");
        assert_eq!(Declined.label(), "DECLINED");
        assert_eq!(Expired.label(), "EXPIRED");
    }

    #[test]
    fn from_label_round_trips() {
        for t in [Offered, Accepted, Declined, Expired] {
            assert_eq!(TransactionState::from_label(t.label()), t);
        }
        assert_eq!(TransactionState::from_label("garbage"), Offered);
    }

    #[test]
    fn accept_only_from_offered() {
        assert!(Offered.can_transition_to(Accepted));
        assert!(Offered.can_transition_to(Declined));
        assert!(Offered.can_transition_to(Expired));
        assert!(!Accepted.can_transition_to(Accepted));
        assert!(!Accepted.can_transition_to(Declined));
        assert!(!Declined.can_transition_to(Accepted));
        assert!(!Expired.can_transition_to(Accepted));
    }

    #[test]
    fn terminal_states_are_dead_ends() {
        for t in [Accepted, Declined, Expired] {
            for to in [Offered, Accepted, Declined, Expired] {
                assert!(!t.can_transition_to(to), "{:?} → {:?}", t, to);
            }
        }
    }

    #[test]
    fn expiry_only_for_unanswered_old_offers() {
        assert!(is_expired(Offered, 25.0));
        assert!(!is_expired(Offered, 23.9));
        assert!(!is_expired(Accepted, 48.0)); // decided offers never lapse
    }

    #[test]
    fn acceptance_rate_math() {
        assert_eq!(acceptance_rate_pct(3, 1, 2), Some(75.0));
        assert_eq!(acceptance_rate_pct(0, 0, 5), None); // expireds don't count
        assert_eq!(acceptance_rate_pct(0, 4, 1), Some(0.0));
    }
}
