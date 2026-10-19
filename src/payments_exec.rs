// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Bill8:Payments execution for accepted upsells.
//!
//! The upgrade payment is an **immutable charge** at Bill8:Payments -
//! hotel8 never shadow-writes ledger balances. The caller supplies the
//! single-use token; it is consumed in exchange for the payment-method
//! id, the charge is created with an idempotency key, and the charge id
//! is returned. Every call presents the `upsell-payments` workload
//! CERTIFICATE in the TLS handshake (CERTFREE-4V - no M2M token
//! exists; the transport is built at boot and stored in the config).

/// POST a charge to Bill8:Payments for an accepted upsell.
///
/// `client` is the boot-built `upsell-payments` certificate transport.
///
/// Returns the charge id on success. Idempotent per
/// `transaction_id` (the charge's idempotency key).
pub async fn process_upgrade(
    client: &reqwest::Client,
    payments_url: &str,
    transaction_id: uuid::Uuid,
    payment_method_token: &str,
    amount: f64,
    currency: &str,
) -> Result<uuid::Uuid, String> {
    // Single-use token → payment_method_id (bill8 PAY-10), then charge.
    let consume = client
        .post(format!(
            "{}/v1/tokens/{}/consume",
            payments_url.trim_end_matches('/'),
            payment_method_token
        ))
        .send()
        .await
        .map_err(|e| format!("Token consume HTTP: {e}"))?;
    if !consume.status().is_success() {
        return Err(format!("Token consume status {}", consume.status()));
    }
    let cv: serde_json::Value = consume
        .json()
        .await
        .map_err(|e| format!("Token consume decode: {e}"))?;
    let payment_method_id: uuid::Uuid = cv
        .get("payment_method_id")
        .and_then(|x| x.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| "Token consume returned no payment_method_id".to_string())?;
    let body = serde_json::json!({
        "payment_method_id": payment_method_id,
        "amount": format!("{amount:.2}"),
        "currency": currency,
        "idempotency_key": format!("hotel8:upsell:{transaction_id}"),
    });
    let resp = client
        .post(format!("{}/v1/charges", payments_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Payments HTTP: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Payments status {}", resp.status()));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Payments decode: {e}"))?;
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| "Payments returned no id".to_string())?;
    Ok(id)
}
