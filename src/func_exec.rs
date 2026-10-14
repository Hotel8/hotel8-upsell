// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Sentin8:Func execution helpers.
//!
//! `run_pricing_module` executes the tenant-deployed `upsell-pricing` WASM
//! module (hook `on_quote`) through the platform Func service — over the
//! certificate-presenting client the boot built from the `upsell` workload
//! identity (CERTFREE-4F; Func's cert-first gate resolves it against the
//! registry map). When no module is deployed (404) or Func is unreachable,
//! it returns `Ok(None)` / `Err` respectively so callers can fall back to
//! the local pure rule.

/// Execute the `upsell-pricing` module's `on_quote` hook.
/// Returns `Ok(None)` when the module is not deployed (404) or the
/// certificate transport is absent at boot (pricing degrades to the
/// local rule).
pub async fn run_pricing_module(
    client: &sentin8_func_sdk::client::FuncClient,
    context: &serde_json::Value,
) -> Result<Option<f64>, String> {
    let req = sentin8_func_sdk::client::ExecuteRequest {
        hook: "on_quote".to_string(),
        context: context.clone(),
        callback_config: None,
    };
    match client.execute("upsell-pricing", &req).await {
        Ok(v) => {
            let price = v.get("suggested_price").and_then(|x| x.as_f64());
            Ok(price)
        }
        Err(sentin8_func_sdk::client::FuncError::Http(ref m)) if m.contains("404") => {
            Ok(None) // module not deployed — caller falls back to the local rule
        }
        Err(e) => Err(format!("Func execute failed: {e}")),
    }
}
