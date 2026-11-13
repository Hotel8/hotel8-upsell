// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Library re-exports for integration tests.
pub mod admin_ui;
pub mod auth;
pub mod config;
pub mod event_consumer;
pub mod event_emit;
pub mod func_exec;
pub mod handlers;
pub mod offer_state;
pub mod openapi;

// The utoipa dump for registration::print - mirrored from main.rs; the
// lib crate compiles registration::print too (test harnesses embedding
// via #[path] never invoke it, so the pointer is referenced, not run).
static OPENAPI_DUMP: fn() -> serde_json::Value = || {
    use utoipa::OpenApi as _;
    serde_json::to_value(openapi::UpsellOpenApi::openapi()).unwrap_or(serde_json::Value::Null)
};

pub mod payments_exec;
pub mod policy;
pub mod pricing;
pub mod registration;
pub mod server;
