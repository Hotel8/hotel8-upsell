// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Transactional event emission (outbox-first) - shared across all handlers.
//!
//! DBX (AGENTS.md "Inbox-first pattern - local outbox + produce API",
//! architecture-principles §6.7): a `upsell.outbox` row MUST be inserted on
//! the **same transaction/connection** as the domain change, and insert
//! errors MUST NOT be silently swallowed. The shell's post-commit relay
//! (boot step 8b) drains the local outbox and pushes each row to Bus
//! `POST /v1/produce` (M2M `sentin8:bus:produce`, idempotency_key = row id)
//! - no BusClient or M2M token is needed on the producer path. A vertical
//! never writes another service's durability tables (`sentin8.bus_inbox` is
//! retired; alignment doc §11-§13 of
//! `sentin8-hotel8-architecture-principles-alignment.md`).
//!
//! Event types use the platform naming convention
//! `vertical:service:resource:operation` - colons only, no dots
//! (`sentin8-sentin8-bus-technical-design.md` §10.1).
//!
//! Publisher identity (bus design §4.5): carries the verified claims'
//! subject + scope of the caller that triggered the domain change, so
//! subscribers can delegate via RFC 8693 token exchange. For background /
//! system operations pass `Publisher::system()`.

/// The  stream family (must equal the service's PG-role-derived family:
/// role `hotel8_upsell_service` → strip `hotel8_` → `upsell`).
pub const STREAM_FAMILY: &str = "upsell";

/// Publisher identity carried on the outbox row (bus design §4.5).
use sentin8::bus::outbox::OutboxError;

#[derive(Debug, Clone, Default)]
pub struct Publisher {
    pub subject: Option<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub scopes: Option<String>,
}

impl Publisher {
    /// From a verified caller (handler path).
    pub fn from_parts(subject: &str, scopes: &str) -> Self {
        Self {
            subject: if subject.is_empty() {
                None
            } else {
                Some(subject.to_string())
            },
            scopes: if scopes.is_empty() {
                None
            } else {
                Some(scopes.to_string())
            },
        }
    }

    /// Background / system operations (no user context).
    #[allow(dead_code)]
    pub fn system() -> Self {
        Self {
            subject: None,
            scopes: None,
        }
    }
}

/// Insert a domain event into the LOCAL outbox `upsell.outbox` **within the
/// caller's transaction/connection** - the same one that performed the
/// domain write (outbox-first). Errors are returned; handlers map them to
/// 500 via `err_db`-style mapping - a silently lost event is a contract
/// violation. The shell's post-commit relay pushes the row to Bus
/// (`POST /v1/produce`, idempotency_key = row id) with the carried publisher
/// subject (§4.5).
///
/// `executor` accepts `&mut PgConnection` or `&mut Transaction<'_, Postgres>`.
pub async fn emit_tx<'e, E>(
    executor: E,
    env_id: uuid::Uuid,
    event_type: &str,
    payload: serde_json::Value,
    publisher: &Publisher,
) -> Result<uuid::Uuid, OutboxError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sentin8::bus::outbox::insert_outbox_row_with_publisher(
        executor,
        env_id,
        STREAM_FAMILY,
        event_type,
        &payload,
        None,
        publisher.subject.as_deref(),
    )
    .await
}

/// Build a self-contained event payload (AGENTS.md payload completeness):
/// carries the ids the consumer needs so it does not have to call back.
#[macro_export]
macro_rules! event_payload {
    ($($json:tt)+) => { serde_json::json!($($json)+) };
}

/// Insert a domain event on a **fresh pool connection** - for handlers with
/// no in-flight domain write (integration stubs, module-config updates).
/// Sets the tenant context on the new connection first (RLS on the outbox
/// requires `env_id` to match). Errors are returned, never swallowed.
#[allow(dead_code)]
pub async fn emit_on_pool(
    pool: &sqlx::PgPool,
    env_id: uuid::Uuid,
    event_type: &str,
    payload: serde_json::Value,
    publisher: &Publisher,
) -> Result<uuid::Uuid, OutboxError> {
    let mut conn = pool.acquire().await.map_err(OutboxError::Db)?;
    sqlx::query(
        "SELECT sentin8_set_tenant_context($1, NULL, NULL, NULL, false, false, false, NULL, $2)",
    )
    .bind(env_id)
    .bind(STREAM_FAMILY)
    .execute(&mut *conn)
    .await
    .map_err(OutboxError::Db)?;
    emit_tx(&mut *conn, env_id, event_type, payload, publisher).await
}

/// Read an inbox id from a row (helper for tests).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_from_parts_roundtrip() {
        let p = Publisher::from_parts("user-1", "hotel8:upsell:reservation:write");
        assert_eq!(p.subject.as_deref(), Some("user-1"));
        assert_eq!(p.scopes.as_deref(), Some("hotel8:upsell:reservation:write"));
        let sys = Publisher::system();
        assert!(sys.subject.is_none() && sys.scopes.is_none());
    }

    #[test]
    fn publisher_empty_becomes_none() {
        let p = Publisher::from_parts("", "");
        assert!(p.subject.is_none() && p.scopes.is_none());
    }
}
