// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Sentin8 Upsell — Upsell Engine service.
//!
//! Boot runs on the platform `sentin8-shell` crate (design
//! `sentin8/design/sentin8-sentin8-shell-technical-design.md`): registration
//! flag, PQ provider, config, capped pool, PG18 check, state binding, OpenAPI
//! merge, middleware stack and PQ-TLS listener are owned by the shell. This
//! binary supplies only its registration data, state and routes.

#[allow(dead_code)]
mod event_consumer;
mod offer_state;
mod func_exec;
mod payments_exec;
mod pricing;
mod event_emit;
mod admin_ui;
mod auth;
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod handlers;
mod openapi;
mod registration;
pub mod policy;
#[allow(dead_code)]
mod server;

use utoipa::OpenApi;


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sentin8::shell::serve(
        sentin8::shell::Sentin8Shell::new("hotel8:upsell", "HOTEL8_UPSELL")
            .with_registration(registration::print)
            .with_state(server::build_state)
            .with_own_health()
            .with_router(|router, _state| server::routes(router))
            .with_openapi(openapi::UpsellOpenApi::openapi()),
    )
    .await
}
