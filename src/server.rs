// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Axum server setup for Upsell.
use std::sync::Arc;
use axum::routing::{get, post, put, delete};
use crate::handlers::AppState;

/// Build a cert-presenting HTTP transport for `workload` (CERTFREE-4V,
/// auth DESIGN §4.3f): mint-or-load the Acme-issued pair and wrap it in
/// a reqwest client. The receiving service's TLS layer authenticates the
/// handshake and the enroll-published registry map authorizes the
/// scopes — no bearer token exists on this path.
///
/// # Errors
/// Returns the certificate-ensure or transport-build failure (the
/// caller logs and degrades).
async fn build_workload_http(
    auth_issuer: &str,
    workload: &str,
) -> anyhow::Result<reqwest::Client> {
    let cert = sentin8::auth::m2m::ensure_workload_certificate(
        auth_issuer,
        workload,
        &sentin8::auth::m2m::platform_ca_path(),
    )
    .await?;
    let mtls = sentin8::middleware::mtls::MtlsConfig::new(
        &cert.ca_cert_path,
        &cert.cert_path,
        &cert.key_path,
    );
    sentin8::middleware::mtls::build_mtls_client(&mtls)
        .map_err(|e| anyhow::anyhow!("workload '{workload}' transport: {e}"))
}

pub async fn build_state(core: sentin8::shell::ServiceCore)
    -> anyhow::Result<crate::handlers::AppState> {
    let mut config = crate::config::UpsellConfig::from_env();

    // Cert-presenting Bus client (CERTFREE-4V): the `upsell` workload
    // CERTIFICATE is the identity — no DCR, no token (auth DESIGN
    // §4.3f); the mint retries while the fleet boots.
    let bus_client = loop {
        match sentin8::bus::client::BusClient::for_workload(
            &config.sentin8_bus_url,
            &config.sentin8_auth_issuer,
            "upsell",
        )
        .await
        {
            Ok(c) => break c,
            Err(e) => {
                tracing::debug!(error = %e, "upsell bus client: certificate not ready yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // PMS cross-service client (CERTFREE-4V): this service presents its
    // own `upsell` workload certificate — PMS's TLS layer authenticates
    // the handshake and the registry map authorizes the scopes (the
    // `pms:reservations:read` / `pms:profiles:read` grants ride the
    // `upsell` map row). The mint retries while the fleet boots.
    let pms_client = loop {
        match hotel8::pms::client::H8pmsClient::with_workload_certificate(
            &config.pms_url,
            &config.sentin8_auth_issuer,
            "upsell",
        )
        .await
        {
            Ok(c) => break c,
            Err(e) => {
                tracing::debug!(error = %e, "upsell pms client: certificate not ready yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // Cross-service payments workload transport (accepted-upsell charges +
    // token consume).
    match build_workload_http(&config.sentin8_auth_issuer, "upsell-payments").await {
        Ok(http) => config.payments_http = Some(http),
        Err(e) => tracing::warn!(error = %e, "upsell-payments transport unavailable — upgrade payments degraded"),
    }

    // CERTFREE-4F: the Func execution lane presents the `upsell`
    // workload certificate (FuncClient::with_workload_identity) — Func's
    // cert-first gate resolves it against the registry map. `None`
    // degrades pricing to the local pure rule (the module path is
    // already optional by design).
    if !config.func_url.is_empty() {
        match sentin8::auth::m2m::ensure_workload_certificate(
            &config.sentin8_auth_issuer,
            "upsell",
            &sentin8::auth::m2m::platform_ca_path(),
        )
        .await
        {
            Ok(cert) => {
                match sentin8_func_sdk::client::FuncClient::with_workload_identity(
                    &config.func_url,
                    &cert.ca_cert_path,
                    &cert.cert_path,
                    &cert.key_path,
                ) {
                    Ok(c) => config.func_client = Some(c),
                    Err(e) => tracing::warn!(error = %e, "upsell func transport build failed — pricing falls back to the local rule"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "upsell func certificate not ready — pricing falls back to the local rule"),
        }
    }

    // ── Sentin8:Policy (Shield) ──
    // Load the service's policy sets; an unparsable policy text REFUSES TO
    // START (fail-closed at boot, design §3.3). Hot-reload consumes the
    // `policy` stream as a direct consumer (design §3.2); deny/would-deny
    // decisions emit to Stat (§4.2f). The engine is stored in AppState so
    // handlers gate via `policy::require` (step 5 is the ONLY gate).
    // ── Sentin8:Policy (Shield, shell-owned boot): consume the
    // engine the shell booted in step 8 — no second engine, consumer, or
    // DCR registration here.
    let policy_engine = core.admin.policy.clone();

    let state = AppState {
        config: Arc::new(config),
        pool: core.pool.clone(),
        bus_client,
        pms_client,
        policy: policy_engine.clone(),
        admin: sentin8::shield::admin::SharedAdminState {
            service: "hotel8:upsell".to_string(),
            pool: core.pool.clone(),
            policy: policy_engine,
            auth_client: Arc::new(core.config.sentin8_auth_client.clone()),
        },
    };

    // Event-driven consumer — subscribes to the PMS stream family.
    crate::event_consumer::spawn(core.pool.clone(), state.bus_client.clone());

    Ok(state)
}

/// Add this service's routes onto the shell-provided router. The shell
/// owns the boot sequence, state binding, OpenAPI merge and the standard
/// middleware stack; this adds ONLY routes (including the service's own
/// `/v1/health` — custom payload).
pub fn routes(router: axum::Router<crate::handlers::AppState>)
    -> axum::Router<crate::handlers::AppState> {
   router
        .route("/v1/health", get(crate::handlers::health))
        // ── Admin UI (RFW) ─────────────────────────────────────────────────
        .route("/v1/admin/ui/{*page}", get(crate::admin_ui::get_admin_ui))
        // ── Sentin8:Policy admin API (shared implementation,
        // handlers generic over this AppState).
        .route("/v1/admin/policy-sets",
            get(sentin8::shield::admin::list_policy_sets::<crate::handlers::AppState>))
        .route("/v1/admin/policy-sets/{name}",
            get(sentin8::shield::admin::get_policy_set::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_policy_set::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_policy_set::<crate::handlers::AppState>))
        .route("/v1/admin/policy-sets/{name}/test",
            post(sentin8::shield::admin::test_policy_set::<crate::handlers::AppState>))
        .route("/v1/admin/policy-sets/enforce",
            post(sentin8::shield::admin::admin_enforce_policy_set::<crate::handlers::AppState>))
        .route("/v1/admin/policy-sets/shadow",
            post(sentin8::shield::admin::admin_shadow_policy_set::<crate::handlers::AppState>))
        .route("/v1/admin/policy-sets/disable",
            post(sentin8::shield::admin::admin_disable_policy_set::<crate::handlers::AppState>))
        .route("/v1/admin/relations",
            get(sentin8::shield::admin::list_relations::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_relation::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_relation::<crate::handlers::AppState>))
        .route("/v1/admin/relations/check",
            get(sentin8::shield::admin::check_relation::<crate::handlers::AppState>))
        .route("/v1/admin/attributes",
            get(sentin8::shield::admin::list_attributes::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_attribute::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_attribute::<crate::handlers::AppState>))
        .route("/v1/offers", post(crate::handlers::create_offer).get(crate::handlers::list_offers))
        .route("/v1/offers/{offer_id}", put(crate::handlers::update_offer))
        .route("/v1/rules", post(crate::handlers::create_rule).get(crate::handlers::list_rules))
        .route("/v1/rules/{rule_id}", delete(crate::handlers::delete_rule))
        .route("/v1/rules/test", post(crate::handlers::test_rule))
        .route("/v1/offers/generate", post(crate::handlers::generate_offers))
        .route("/v1/offers/for-reservation", get(crate::handlers::get_offers_for_reservation))
        .route("/v1/offers/accept", post(crate::handlers::accept_offer))
        .route("/v1/offers/decline", post(crate::handlers::decline_offer))
        .route("/v1/offers/status", get(crate::handlers::get_offer_status))
        .route("/v1/analytics", get(crate::handlers::get_analytics))
        .route("/v1/offers/expire", post(crate::handlers::expire_offers))
        // Eligibility types
        .route("/v1/eligibility/types", get(crate::handlers::list_eligibility_types))
        // Offer configuration
        .route("/v1/offers/{offer_id}/configure", put(crate::handlers::configure_offer))
        // Cron daily generation
        .route("/v1/cron/daily-generation", post(crate::handlers::cron_daily_generation))
        // PMS integration
        .route("/v1/integration/pms", get(crate::handlers::fetch_hotel8_pms_data))
        // Func dynamic pricing
        .route("/v1/integration/func/dynamic-price", post(crate::handlers::compute_dynamic_price))
        // Post notification
        .route("/v1/integration/post/notify", post(crate::handlers::send_offer_notification))
        // Payments payment
        .route("/v1/integration/payments/payment", post(crate::handlers::process_upgrade_payment))
        // PMS reservation modification
        .route("/v1/integration/pms/modify-reservation", post(crate::handlers::modify_reservation_on_acceptance))
        // Post confirmation
        .route("/v1/integration/post/confirmation", post(crate::handlers::send_acceptance_confirmation))
        // A/B testing
        .route("/v1/ab-tests", post(crate::handlers::create_ab_test))
        .route("/v1/ab-tests/results", get(crate::handlers::get_ab_test_results))
        // Rate limiting config
        .route("/v1/rate-limits", get(crate::handlers::get_rate_limit_config))
}

/// Shared policy-admin surface accessor — the shared
/// handlers extract this service's admin state via `HasPolicyAdmin`.
impl sentin8::shield::admin::HasPolicyAdmin for crate::handlers::AppState {
    fn policy_admin(&self) -> &sentin8::shield::admin::SharedAdminState {
        &self.admin
    }
}
