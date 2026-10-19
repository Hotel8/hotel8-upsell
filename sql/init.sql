-- SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
-- SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
-- SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
-- SPDX-License-Identifier: Apache-2.0

-- Hotel8:Upsell - Upsell Engine tables.
CREATE SCHEMA IF NOT EXISTS upsell;
DO $$ BEGIN
    CREATE TYPE upsell.offer_status AS ENUM ('DRAFT', 'ACTIVE', 'PAUSED', 'EXPIRED', 'ARCHIVED');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

CREATE TABLE IF NOT EXISTS upsell.offers (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id  UUID NOT NULL,
    env_id       UUID NOT NULL,
    offer_code   TEXT NOT NULL,
    offer_name   TEXT NOT NULL,
    description  TEXT,
    price        NUMERIC(12,2) NOT NULL DEFAULT 0,
    category     TEXT NOT NULL DEFAULT 'UPGRADE',
    status       upsell.offer_status NOT NULL DEFAULT 'DRAFT',
    start_date   DATE,
    end_date     DATE,
    min_los      INTEGER,
    max_advance_booking INTEGER,
    target_room_types JSONB NOT NULL DEFAULT '[]',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_upsell_offers_property ON upsell.offers (property_id);
CREATE INDEX IF NOT EXISTS idx_upsell_offers_env ON upsell.offers (env_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_upsell_offers_code ON upsell.offers (property_id, offer_code);

CREATE TABLE IF NOT EXISTS upsell.eligibility_rules (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    offer_id     UUID NOT NULL REFERENCES upsell.offers(id),
    rule_type    TEXT NOT NULL,
    rule_value   JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_upsell_er_offer ON upsell.eligibility_rules (offer_id);

CREATE TABLE IF NOT EXISTS upsell.upsell_transactions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id    UUID NOT NULL,
    env_id         UUID NOT NULL,
    reservation_id UUID,
    offer_id       UUID REFERENCES upsell.offers(id),
    guest_profile_id UUID,
    status         TEXT NOT NULL DEFAULT 'OFFERED',
    accepted_at    TIMESTAMPTZ,
    declined_at    TIMESTAMPTZ,
    price_charged  NUMERIC(12,2),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_upsell_ut_property ON upsell.upsell_transactions (property_id);
CREATE INDEX IF NOT EXISTS idx_upsell_ut_reservation ON upsell.upsell_transactions (reservation_id) WHERE reservation_id IS NOT NULL;

ALTER TABLE upsell.offers ENABLE ROW LEVEL SECURITY;
ALTER TABLE upsell.offers FORCE ROW LEVEL SECURITY;
ALTER TABLE upsell.eligibility_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE upsell.eligibility_rules FORCE ROW LEVEL SECURITY;
ALTER TABLE upsell.upsell_transactions ENABLE ROW LEVEL SECURITY;
ALTER TABLE upsell.upsell_transactions FORCE ROW LEVEL SECURITY;
-- env_id is the sole tenant key (architecture-principles.md §4.3).
DROP POLICY IF EXISTS upsell_offers_rls ON upsell.offers;
CREATE POLICY upsell_offers_rls ON upsell.offers USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS upsell_er_rls ON upsell.eligibility_rules;
CREATE POLICY upsell_er_rls ON upsell.eligibility_rules USING (offer_id IN (SELECT id FROM upsell.offers));
DROP POLICY IF EXISTS upsell_ut_rls ON upsell.upsell_transactions;
CREATE POLICY upsell_ut_rls ON upsell.upsell_transactions USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
-- ──────────────────────────────────────────────────────────────────────────
-- Local outbox (DBX, architecture-principles §6.7) - producer path:
-- rows are appended in the SAME transaction as the domain change; the
-- shell's post-commit relay pushes them to Bus POST /v1/produce
-- (M2M sentin8:bus:produce, idempotency_key = row id).
-- Generated from sentin8_outbox::outbox_table_sql("upsell").
-- ──────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS upsell.outbox (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    event_type      TEXT NOT NULL,
    stream_family   TEXT NOT NULL,
    payload         JSONB NOT NULL,
    publisher_id_token  TEXT,
    publisher_subject   TEXT,
    relayed         BOOLEAN NOT NULL DEFAULT false,
    relayed_at      TIMESTAMPTZ,
    relay_attempts  INT NOT NULL DEFAULT 0,
    next_retry_at   TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_outbox_unrelayed
    ON upsell.outbox (created_at) WHERE relayed = false;
ALTER TABLE upsell.outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE upsell.outbox FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS outbox_select ON upsell.outbox;
CREATE POLICY outbox_select ON upsell.outbox FOR SELECT
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS outbox_insert ON upsell.outbox;
CREATE POLICY outbox_insert ON upsell.outbox FOR INSERT
    WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS outbox_update ON upsell.outbox;
CREATE POLICY outbox_update ON upsell.outbox FOR UPDATE
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
CREATE OR REPLACE FUNCTION upsell.upsell_notify_outbox() RETURNS TRIGGER
    LANGUAGE plpgsql AS $fn$
BEGIN
    PERFORM pg_notify('upsell_outbox_changed', '');
    RETURN NEW;
END;
$fn$;
DROP TRIGGER IF EXISTS outbox_notify_trigger ON upsell.outbox;
CREATE TRIGGER outbox_notify_trigger
    AFTER INSERT ON upsell.outbox
    FOR EACH ROW
    EXECUTE FUNCTION upsell.upsell_notify_outbox();

-- ──────────────────────────────────────────────────────────────────────────
-- Local audit journal (architecture-principles §6.8): same-transaction
-- journal rows; the shell's post-commit pusher ships them to Sys
-- POST /v1/audit (M2M sentin8:sys:audit:write). A Sys outage only delays
-- delivery - rows persist until pushed.
-- Generated from sentin8_audit::audit_journal_sql("upsell").
-- ──────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS upsell.audit_journal (
    id              UUID PRIMARY KEY,
    env_id          UUID NOT NULL,
    action          TEXT NOT NULL,
    target_resource_type TEXT,
    target_resource_id    TEXT,
    actor_user_id   TEXT,
    actor_subject_type TEXT,
    severity        TEXT NOT NULL DEFAULT 'info',
    correlation_id  TEXT,
    before_state    JSONB,
    after_state     JSONB,
    details         JSONB,
    pushed          BOOLEAN NOT NULL DEFAULT false,
    pushed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_upsell_audit_journal_unpushed
    ON upsell.audit_journal (created_at) WHERE pushed = false;
ALTER TABLE upsell.audit_journal ENABLE ROW LEVEL SECURITY;
ALTER TABLE upsell.audit_journal FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS audit_journal_dml ON upsell.audit_journal;
CREATE POLICY audit_journal_dml ON upsell.audit_journal
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())
    WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id());

-- ──────────────────────────────────────────────────────────────────────────
-- Per-service role grants + search_path (§4.3e / VDC §VDC.3):
-- hotel8_{leaf}_service connects via PostgreSQL 18 OAuth (scope→role map in
-- hotel8.sys.pg_oauth_scope_map). search_path puts the service's OWN schema
-- FIRST (outbox/audit_journal resolution for the shell's relay + journal
-- pusher; unqualified domain tables resolve here), then public (RLS helpers).
-- `sentin8-cli vertical enroll`'s never-clobber block preserves this role-level setting.
-- ──────────────────────────────────────────────────────────────────────────
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'hotel8_upsell_service') THEN
        ALTER ROLE hotel8_upsell_service SET search_path = upsell, public;
        GRANT USAGE ON SCHEMA upsell TO hotel8_upsell_service;
        GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA upsell TO hotel8_upsell_service;
        GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA upsell TO hotel8_upsell_service;
    END IF;
END $$;


-- ═════════════════════════════════════════════════════════════════════════════
-- From migrations: extended
-- ═══════════════════════════════════════════════════════════════════════════
-- Migration 31: Hotel8:Upsell extended columns for remaining H2 stories
-- H2.2.3: offer configuration (pricing strategy, upgraded room type, currency)
-- H2.4.5: reservation modification tracking
-- H2.6.2: offer expiry

CREATE SCHEMA IF NOT EXISTS upsell;
ALTER TABLE upsell.offers
    ADD COLUMN IF NOT EXISTS pricing_strategy TEXT NOT NULL DEFAULT 'FIXED',
    ADD COLUMN IF NOT EXISTS upgraded_room_type TEXT,
    ADD COLUMN IF NOT EXISTS currency_code TEXT NOT NULL DEFAULT 'USD';

ALTER TABLE upsell.upsell_transactions
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS variant_label TEXT,
    ADD COLUMN IF NOT EXISTS modified_reservation JSONB;

-- H2.5.2: A/B testing - variant tracking
ALTER TABLE upsell.eligibility_rules
    ADD COLUMN IF NOT EXISTS variant_label TEXT,
    ADD COLUMN IF NOT EXISTS is_active BOOLEAN NOT NULL DEFAULT true;

CREATE INDEX IF NOT EXISTS idx_upsell_ut_expires ON upsell.upsell_transactions (expires_at) WHERE expires_at IS NOT NULL;
