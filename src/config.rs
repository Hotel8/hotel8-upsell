// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Upsell configuration.
use sentin8::auth::client::{Client as AuthClient, ClientOptions};
use std::env;

/// Manual `Debug` - `FuncClient` (and the Auth client) carry transport
/// internals that must never log.
#[derive(Clone)]
pub struct UpsellConfig {
    pub port: u16,
    pub database_url: String,
    pub sentin8_auth_issuer: String,
    pub sentin8_auth_jwks_url: String,
    pub sentin8_bus_url: String,
    pub pms_url: String,
    pub func_url: String,
    /// Certificate-presenting Func client (CERTFREE-4F - the `upsell`
    /// workload certificate presents in the TLS handshake; no bearer
    /// token exists on the path). Built at boot; `None` when the
    /// certificate is not yet mintable (the caller degrades to the
    /// local pricing rule).
    pub func_client: Option<sentin8_func_sdk::client::FuncClient>,
    pub bill8_payments_url: String,
    pub sentin8_auth_client: AuthClient,
    /// Certificate-presenting transport for the `upsell-payments` workload
    /// (accepted-upsell charges + token consume - CERTFREE-4V, no M2M
    /// token exists).
    pub payments_http: Option<reqwest::Client>,
}

impl UpsellConfig {
    pub fn from_env() -> Self {
        let cfg = sentin8::config::Sentin8Config::from_env();
        Self {
            port: env::var("HOTEL8_UPSELL_PORT")
                .expect("HOTEL8_UPSELL_PORT must be set by captain")
                .parse()
                .expect("HOTEL8_UPSELL_PORT must be a number"),
            // DBX (§VDC.3) - required DSN, vertical database (hotel8), no
            // platform-database fallback.
            database_url: env::var("HOTEL8_UPSELL_DATABASE_URL").unwrap_or_else(|_| {
                "postgres://hotel8_upsell_service@localhost:5432/hotel8".into()
            }),
            sentin8_auth_issuer: env::var("SENTIN8_AUTH_ISSUER")
                .unwrap_or_else(|_| cfg.sentin8_auth_issuer()),
            sentin8_auth_jwks_url: env::var("SENTIN8_AUTH_JWKS_URL")
                .unwrap_or_else(|_| cfg.sentin8_auth_jwks_url()),
            sentin8_bus_url: env::var("SENTIN8_BUS_URL").unwrap_or_else(|_| cfg.sentin8_bus_url()),
            pms_url: env::var("HOTEL8_PMS_URL")
                .unwrap_or_else(|_| format!("https://{}", cfg.platform_domain("hotel8:pms"))),
            func_url: env::var("SENTIN8_FUNC_URL").unwrap_or_else(|_| cfg.sentin8_func_url()),
            func_client: None,
            bill8_payments_url: env::var("BILL8_PAYMENTS_URL")
                .unwrap_or_else(|_| format!("https://{}", cfg.platform_domain("bill8:payments"))),
            sentin8_auth_client: AuthClient::new(ClientOptions {
                base_url: env::var("SENTIN8_AUTH_ISSUER")
                    .unwrap_or_else(|_| cfg.sentin8_auth_issuer()),
                ..Default::default()
            }),
            payments_http: None,
        }
    }
}
impl std::fmt::Debug for UpsellConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpsellConfig")
            .field("port", &self.port)
            .field("database_url", &"<redacted>")
            .field("pms_url", &self.pms_url)
            .field("func_url", &self.func_url)
            .field("func_client", &self.func_client.is_some())
            .field("bill8_payments_url", &self.bill8_payments_url)
            .finish_non_exhaustive()
    }
}
