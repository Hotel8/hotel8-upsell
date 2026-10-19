// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Sentin8:Bus consumer - reacts to PMS checkout events.
//!
//! Reaction is **event-driven**: Hotel8:PMS emits
//! `hotel8:pms:reservation:checked_out` (stream family `pms`) when a guest
//! checks out. This consumer subscribes to the `pms` family via SSE, parses
//! the checkout payload and offer generation on PMS reservation creation.
//!
//! Uses the platform SSE consumer pattern (bus-client `sse_consume` +
//! `POST /v1/ack`) - the platform's env-init consumer shape.
//!
//! Design: `hotel8/design/sentin8-hotel8-upsell-technical-design.md` §7
//! (A6 event consumers).

use std::time::Duration;

use futures_util::StreamExt;
use sqlx::PgPool;

/// Consumer group - load-balances across upsell replicas; a message is
/// handled once per group (at-least-once + idempotent award = effectively-once).
const CONSUMER_GROUP: &str = "hotel8:upsell:offers";

/// Reconnect delay when the Bus SSE connection drops or an error occurs.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Event handled by this consumer.
pub const EVENT_CHECKED_OUT: &str = "hotel8:pms:reservation:created";

/// Spawn the Bus consumer background task. Called once from `server.rs`.
pub fn spawn(
    pool: PgPool,
    bus_client: sentin8::bus::client::BusClient,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run(pool, bus_client).await;
    })
}

/// Run the consumer loop: connect to Bus SSE for the `pms` stream family,
/// read messages, dispatch relevant events, ACK on success, reconnect on
/// disconnect or error.
async fn run(pool: PgPool, bus_client: sentin8::bus::client::BusClient) {
    tracing::info!("upsell: bus consumer started (consumer_group={CONSUMER_GROUP})");
    loop {
        match bus_client.sse_consume("pms", CONSUMER_GROUP).await {
            Ok(resp) => {
                tracing::info!("upsell: bus SSE connected to stream family 'pms'");
                sse_loop(&pool, &bus_client, resp).await;
                tracing::info!("upsell: bus SSE disconnected - reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "upsell: bus SSE consume failed - reconnecting");
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Process messages from a single Bus SSE connection until it drops.
async fn sse_loop(
    pool: &PgPool,
    bus_client: &sentin8::bus::client::BusClient,
    resp: reqwest::Response,
) {
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "upsell: bus SSE stream error");
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // SSE events are separated by blank lines; each carries `data:` lines.
        while let Some(blank_pos) = buffer.find("\n\n") {
            let event_str = buffer[..blank_pos].to_string();
            buffer = buffer[blank_pos + 2..].to_string();

            let data_line = event_str
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(|s| s.trim_start()))
                .collect::<Vec<_>>()
                .join("");
            if data_line.is_empty() {
                continue;
            }

            let msg: sentin8::bus::contracts::Message = match serde_json::from_str(&data_line) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "upsell: failed to parse SSE message");
                    continue;
                }
            };

            handle_message(pool, bus_client, &msg).await;

            if let Err(e) = bus_client.ack("pms", CONSUMER_GROUP, &msg.message_id).await {
                tracing::warn!(error = %e, message_id = %msg.message_id, "upsell: ack failed");
            }
        }
    }
}

/// Handle a single bus message: award points on PMS checkout.
async fn handle_message(
    pool: &PgPool,
    _bus_client: &sentin8::bus::client::BusClient,
    msg: &sentin8::bus::contracts::Message,
) {
    if msg.event_type != EVENT_CHECKED_OUT {
        return; // other pms events are not upsell concerns
    }
    // Payload (event-completeness, A1): reservation_id + room_number (+ env_id
    // in the envelope). Points awarding needs the folio spend - that query
    // belongs to PMS; the award here uses the stay-nights rule extracted in
    // `points_logic` (pure, DB-free) over the reservation window.
    let reservation_id = msg
        .payload
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let _room_number = msg
        .payload
        .get("room_number")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if reservation_id.is_empty() {
        tracing::warn!("upsell: checkout event missing reservation_id - skipped");
        return;
    }
    tracing::info!(reservation_id, env_id = %msg.env_id, "upsell: checkout event received");
    // Idempotent award: the award INSERT dedupes on (member, reservation) -
    // see `points_logic::award_on_checkout_sql`. Replays are no-ops.
    let _ = pool; // award execution wired in the D-layer story (member lookup by folio)
}
