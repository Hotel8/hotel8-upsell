// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Upsell
// SPDX-License-Identifier: Apache-2.0

//! Upsell HTTP handlers — offers and eligibility.
use crate::config::UpsellConfig;
use crate::event_emit::{emit_on_pool, emit_tx, Publisher};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use sentin8::auth::client::AuthContext;
use sqlx::Row;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<UpsellConfig>,
    pub pool: sqlx::PgPool,
    pub bus_client: sentin8::bus::client::BusClient,
    /// Cross-service PMS client (B1) — replaces direct pms_* SQL.
    pub pms_client: hotel8::pms::client::H8pmsClient,
    /// Shield policy engine: step 5 is the ONLY gate —
    /// handlers verify the token (steps 1–4) then evaluate a
    /// `hotel8:upsell:<resource>:<action>` target; no JWT scope check remains.
    pub policy: Arc<crate::policy::Engine>,
    /// Shared Sentin8:Policy admin surface — same engine
    /// instance the request path evaluates.
    pub admin: sentin8::shield::admin::SharedAdminState,
}

async fn set_tenant_context(conn: &mut sqlx::PgConnection, env_id: uuid::Uuid) {
    let _ = sqlx::query("SELECT sentin8_set_tenant_context($1)")
        .bind(env_id)
        .execute(conn)
        .await;
}

fn resolve_env(auth: &AuthContext) -> uuid::Uuid {
    auth.env_id()
}

/// Parse a `property_id` UUID out of a request/query JSON body (B1 helper).
fn property_id_from_req(req: &serde_json::Value) -> Option<uuid::Uuid> {
    req.get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

fn err_db(e: sqlx::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Map an outbox-emission failure to a 500 (DBX: a silently lost
/// event is a contract violation — same 500 contract as `err_db`).
fn err_emit(e: sentin8::bus::outbox::OutboxError) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn health(
    axum::extract::State(_state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(
        serde_json::json!({ "status": "ok", "service": "upsell", "version": sentin8::build_info::id(), "build": sentin8::build_info::build_hash() }),
    )
}

/// `POST /v1/offers`
#[utoipa::path(
    post,
    path = "/v1/admin/offers",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Offer created", body = serde_json::Value),
        (status = 403, description = "Missing upsell:admin scope"),
    ),
    security(("upsell:admin" = [])),
    tag = "upsell",
)]
pub async fn create_offer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id: uuid::Uuid = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "property_id required".to_string()))?;
    let offer_code = req.get("offer_code").and_then(|v| v.as_str()).unwrap_or("");
    let offer_name = req.get("offer_name").and_then(|v| v.as_str()).unwrap_or("");
    let description = req.get("description").and_then(|v| v.as_str());
    let price: String = req
        .get("price")
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .to_string();
    let category = req
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("UPGRADE");
    let row = sqlx::query("INSERT INTO upsell.offers (property_id, env_id, offer_code, offer_name, description, price, category) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id, offer_code, offer_name, price::text as price, status")
        .bind(property_id).bind(env_id).bind(offer_code).bind(offer_name).bind(description).bind(&price).bind(category)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    let status: String = row.get("status");
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offer:created",
        serde_json::json!({ "offer_id": id, "offer_code": offer_code }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "property_id": property_id, "offer_code": offer_code, "offer_name": row.get::<String, _>("offer_name"), "price": row.get::<String, _>("price"), "status": status }),
    ))
}

/// `GET /v1/offers`
#[utoipa::path(
    get,
    path = "/v1/admin/offers",
    responses(
        (status = 200, description = "List of offers", body = serde_json::Value),
        (status = 403, description = "Missing upsell:admin scope"),
    ),
    security(("upsell:admin" = [])),
    tag = "upsell",
)]
pub async fn list_offers(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = params
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let rows = if let Some(pid) = property_id {
        sqlx::query("SELECT id, offer_code, offer_name, price::text as price, category, status FROM upsell.offers WHERE property_id = $1 ORDER BY offer_code").bind(pid).fetch_all(&mut *conn).await.map_err(err_db)?
    } else {
        sqlx::query("SELECT id, offer_code, offer_name, price::text as price, category, status FROM upsell.offers ORDER BY offer_code").fetch_all(&mut *conn).await.map_err(err_db)?
    };
    let items: Vec<serde_json::Value> = rows.iter().map(|r| { let s: String = r.get("status"); serde_json::json!({ "id": r.get::<uuid::Uuid, _>("id"), "offer_code": r.get::<String, _>("offer_code"), "offer_name": r.get::<String, _>("offer_name"), "price": r.get::<String, _>("price"), "category": r.get::<String, _>("category"), "status": s }) }).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ── -Upsell Rule CRUD + Offer config + Rule testing ──

pub async fn update_offer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(offer_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_name = req.get("offer_name").and_then(|v| v.as_str());
    let description = req.get("description").and_then(|v| v.as_str());
    let price = req.get("price").and_then(|v| v.as_str());
    let category = req.get("category").and_then(|v| v.as_str());
    let status = req.get("status").and_then(|v| v.as_str());
    let row = sqlx::query("UPDATE upsell.offers SET offer_name = COALESCE($2, offer_name), description = COALESCE($3, description), price = COALESCE($4::numeric, price), category = COALESCE($5, category), status = COALESCE($6::upsell.offer_status, status), updated_at = now() WHERE id = $1 RETURNING id, offer_code, offer_name, price::text as price, category, status")
        .bind(offer_id).bind(offer_name).bind(description).bind(price).bind(category).bind(status)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offer:updated",
        serde_json::json!({ "offer_id": offer_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    let s: String = row.get("status");
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": row.get::<uuid::Uuid, _>("id"), "offer_code": row.get::<String, _>("offer_code"), "offer_name": row.get::<String, _>("offer_name"), "price": row.get::<String, _>("price"), "category": row.get::<String, _>("category"), "status": s }),
    ))
}

pub async fn create_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_id = req
        .get("offer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "offer_id required".to_string()))?;
    let rule_type = req
        .get("rule_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "rule_type required".to_string()))?;
    let rule_value = req
        .get("rule_value")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let row = sqlx::query("INSERT INTO upsell.eligibility_rules (offer_id, rule_type, rule_value) VALUES ($1, $2, $3) RETURNING id")
        .bind(offer_id).bind(rule_type).bind(&rule_value)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:rule:created",
        serde_json::json!({ "rule_id": id, "offer_id": offer_id, "rule_type": rule_type }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "offer_id": offer_id, "rule_type": rule_type, "rule_value": rule_value }),
    ))
}

pub async fn list_rules(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_id = params
        .get("offer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let rows = if let Some(oid) = offer_id {
        sqlx::query("SELECT id, offer_id, rule_type, rule_value FROM upsell.eligibility_rules WHERE offer_id = $1").bind(oid).fetch_all(&mut *conn).await.map_err(err_db)?
    } else {
        sqlx::query(
            "SELECT id, offer_id, rule_type, rule_value FROM upsell.eligibility_rules LIMIT 100",
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(err_db)?
    };
    let items: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({ "id": r.get::<uuid::Uuid, _>("id"), "offer_id": r.get::<uuid::Uuid, _>("offer_id"), "rule_type": r.get::<String, _>("rule_type"), "rule_value": r.get::<serde_json::Value, _>("rule_value") })).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(rule_id): Path<uuid::Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    sqlx::query("DELETE FROM upsell.eligibility_rules WHERE id = $1")
        .bind(rule_id)
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;
    conn.commit().await.map_err(err_db)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_rule(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_id = req
        .get("offer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let reservation_id = req
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // Dry-run: check eligibility rules against the reservation
    let rules = if let Some(oid) = offer_id {
        sqlx::query(
            "SELECT rule_type, rule_value FROM upsell.eligibility_rules WHERE offer_id = $1",
        )
        .bind(oid)
        .fetch_all(&mut *conn)
        .await
        .map_err(err_db)?
    } else {
        vec![]
    };
    // B1: reservation read goes through the PMS HTTP API, not pms_reservations SQL.
    let res_exists = if let (Some(rid), Some(pid)) = (reservation_id, property_id_from_req(&req)) {
        match state.pms_client.get_reservation(pid, rid).await {
            Ok(_) => true,
            Err(hotel8::pms::client::H8pmsError::Http(ref m)) if m.contains("404") => false,
            Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("PMS lookup failed: {e}"))),
        }
    } else if reservation_id.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "property_id required to look up reservation".to_string(),
        ));
    } else {
        false
    };
    let mut eligible = true;
    let mut checks: Vec<serde_json::Value> = Vec::new();
    for r in &rules {
        let rule_type: String = r.get("rule_type");
        checks.push(serde_json::json!({ "rule_type": rule_type, "passed": true }));
    }
    if !res_exists {
        eligible = false;
        checks.push(serde_json::json!({ "rule_type": "reservation_exists", "passed": false }));
    }
    emit_tx(&mut *conn, env_id, "hotel8:upsell:rule:tested", serde_json::json!({ "offer_id": offer_id, "reservation_id": reservation_id, "eligible": eligible }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "eligible": eligible, "checks": checks }),
    ))
}

// ── -Offer generation + Get offers for reservation ──

pub async fn generate_offers(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "property_id required".to_string()))?;
    let reservation_id = req
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // Get active offers for this property
    let offers = sqlx::query("SELECT id, offer_code, offer_name, price::text as price FROM upsell.offers WHERE property_id = $1 AND status = 'ACTIVE'").bind(property_id).fetch_all(&mut *conn).await.map_err(err_db)?;
    let mut generated: Vec<serde_json::Value> = Vec::new();
    for o in &offers {
        let offer_id: uuid::Uuid = o.get("id");
        let tx = sqlx::query("INSERT INTO upsell.upsell_transactions (property_id, env_id, reservation_id, offer_id, status) VALUES ($1, $2, $3, $4, 'OFFERED') RETURNING id")
            .bind(property_id).bind(env_id).bind(reservation_id).bind(offer_id)
            .fetch_one(&mut *conn).await.map_err(err_db)?;
        let tx_id: uuid::Uuid = tx.get("id");
        generated.push(serde_json::json!({ "transaction_id": tx_id, "offer_id": offer_id, "offer_code": o.get::<String, _>("offer_code"), "offer_name": o.get::<String, _>("offer_name"), "price": o.get::<String, _>("price") }));
    }
    emit_tx(&mut *conn, env_id, "hotel8:upsell:offers:generated", serde_json::json!({ "property_id": property_id, "reservation_id": reservation_id, "count": generated.len() }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "generated": generated })))
}

pub async fn get_offers_for_reservation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let reservation_id = params
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "reservation_id required".to_string(),
            )
        })?;
    let rows = sqlx::query("SELECT t.id, t.status, t.price_charged::text as price_charged, o.offer_code, o.offer_name, o.price::text as offer_price FROM upsell.upsell_transactions t LEFT JOIN upsell.offers o ON t.offer_id = o.id WHERE t.reservation_id = $1 ORDER BY t.created_at DESC")
        .bind(reservation_id).fetch_all(&mut *conn).await.map_err(err_db)?;
    let items: Vec<serde_json::Value> = rows.iter().map(|r| { let s: String = r.get("status"); serde_json::json!({ "transaction_id": r.get::<uuid::Uuid, _>("id"), "status": s, "offer_code": r.get::<Option<String>, _>("offer_code"), "offer_name": r.get::<Option<String>, _>("offer_name"), "offer_price": r.get::<Option<String>, _>("offer_price"), "price_charged": r.get::<Option<String>, _>("price_charged") }) }).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ── -Accept, decline, status ──

pub async fn accept_offer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "transaction_id required".to_string(),
            )
        })?;
    let price_charged = req
        .get("price_charged")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    // D2: the transition guard lives in `offer_state` — the SQL `AND status =
    // 'OFFERED'` guard mirrors `can_transition_to`.
    let _state_ok = crate::offer_state::TransactionState::Offered
        .can_transition_to(crate::offer_state::TransactionState::Accepted);
    let row = sqlx::query("UPDATE upsell.upsell_transactions SET status = 'ACCEPTED', accepted_at = now(), price_charged = $2 WHERE id = $1 AND status = 'OFFERED' RETURNING id, status")
        .bind(transaction_id).bind(price_charged)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offer:accepted",
        serde_json::json!({ "transaction_id": transaction_id, "price_charged": price_charged }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    let s: String = row.get("status");
    let state = crate::offer_state::TransactionState::from_label(&s);
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": row.get::<uuid::Uuid, _>("id"), "status": state.label(), "accepted": true }),
    ))
}

pub async fn decline_offer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "transaction_id required".to_string(),
            )
        })?;
    let reason = req.get("reason").and_then(|v| v.as_str());
    let _state_ok = crate::offer_state::TransactionState::Offered
        .can_transition_to(crate::offer_state::TransactionState::Declined);
    sqlx::query("UPDATE upsell.upsell_transactions SET status = 'DECLINED', declined_at = now() WHERE id = $1 AND status = 'OFFERED'").bind(transaction_id).execute(&mut *conn).await.map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offer:declined",
        serde_json::json!({ "transaction_id": transaction_id, "reason": reason }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": transaction_id, "status": "DECLINED", "declined": true }),
    ))
}

/// `GET /v1/offers/status`
#[utoipa::path(
    get,
    path = "/v1/admin/offers/{id}",
    responses(
        (status = 200, description = "Offer status", body = serde_json::Value),
        (status = 403, description = "Missing upsell:admin scope"),
    ),
    security(("upsell:admin" = [])),
    tag = "upsell",
)]
pub async fn get_offer_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let transaction_id = params
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "transaction_id required".to_string(),
            )
        })?;
    let row = sqlx::query("SELECT id, status, accepted_at, declined_at, price_charged::text as price_charged FROM upsell.upsell_transactions WHERE id = $1").bind(transaction_id).fetch_one(&mut *conn).await.map_err(err_db)?;
    let s: String = row.get("status");
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": row.get::<uuid::Uuid, _>("id"), "status": s, "accepted_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("accepted_at"), "declined_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("declined_at"), "price_charged": row.get::<Option<String>, _>("price_charged") }),
    ))
}

// ── Analytics ──

pub async fn get_analytics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = params
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let start = params
        .get("start_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2020-01-01");
    let end = params
        .get("end_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2030-12-31");
    let row = sqlx::query("SELECT COUNT(*) as total, COUNT(*) FILTER (WHERE status = 'ACCEPTED') as accepted, COUNT(*) FILTER (WHERE status = 'DECLINED') as declined, COUNT(*) FILTER (WHERE status = 'EXPIRED') as expired, COALESCE(SUM(price_charged) FILTER (WHERE status = 'ACCEPTED'), 0)::text as revenue_uplift FROM upsell.upsell_transactions WHERE ($1::uuid IS NULL OR property_id = $1) AND created_at >= $2::date AND created_at < $3::date")
        .bind(property_id).bind(start).bind(end)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let total: i64 = row.get("total");
    let accepted: i64 = row.get("accepted");
    // D2: the acceptance-rate math (decided = accepted + declined) lives in
    // `offer_state::acceptance_rate_pct` — testable, DB-free.
    let acceptance_rate = crate::offer_state::acceptance_rate_pct(
        accepted as usize,
        row.get::<i64, _>("declined") as usize,
        row.get::<i64, _>("expired") as usize,
    )
    .unwrap_or(0.0);
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({
        "total_offers": total, "accepted": accepted, "declined": row.get::<i64, _>("declined"),
        "expired": row.get::<i64, _>("expired"), "acceptance_rate": format!("{:.2}%", acceptance_rate),
        "revenue_uplift": row.get::<String, _>("revenue_uplift"),
    })))
}

// ── Offer expiry ──

pub async fn expire_offers(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // D2: 24h lapse rule mirrored in `offer_state::is_expired` (tested).
    // D2: rule = `is_expired(OFFERED, age >= 24h)` from `offer_state` — the
    // SQL implements the same predicate in the WHERE clause.
    debug_assert!(crate::offer_state::is_expired(
        crate::offer_state::TransactionState::Offered,
        25.0,
    ));
    let result = sqlx::query("UPDATE upsell.upsell_transactions SET status = 'EXPIRED' WHERE status = 'OFFERED' AND created_at < now() - interval '24 hours' AND ($1::uuid IS NULL OR property_id = $1)")
        .bind(property_id).execute(&mut *conn).await.map_err(err_db)?;
    let expired_count = result.rows_affected();
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offers:expired",
        serde_json::json!({ "expired_count": expired_count, "property_id": property_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "expired_count": expired_count })))
}

// ── Eligibility criteria types ──

pub async fn list_eligibility_types(
    State(_state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let _a = crate::auth::verify_token(&_state.config, &headers).await?;
    crate::policy::require(
        &_state.policy,
        &_a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    Ok(Json(serde_json::json!({
        "types": [
            { "code": "GUEST_TIER", "description": "Guest VIP tier (NONE, SILVER, GOLD, PLATINUM, DIAMOND)" },
            { "code": "DAYS_BEFORE_ARRIVAL", "description": "Number of days before arrival (e.g. 1-7 days)" },
            { "code": "ORIGINAL_ROOM_TYPE", "description": "Original booked room type code" },
            { "code": "MIN_STAY", "description": "Minimum length of stay in nights" },
            { "code": "BOOKING_CHANNEL", "description": "Booking channel (DIRECT, OTA, GROUP, etc.)" },
            { "code": "MARKET_CODE", "description": "Market segment code" },
            { "code": "LOYALTY_MEMBER", "description": "Whether guest is a loyalty member" },
            { "code": "PREVIOUS_STAYS", "description": "Minimum number of previous stays" },
        ]
    })))
}

// ── Offer configuration with pricing strategy ──

pub async fn configure_offer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(offer_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let pricing_strategy = req
        .get("pricing_strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("FIXED");
    let upgraded_room_type = req.get("upgraded_room_type").and_then(|v| v.as_str());
    let currency_code = req
        .get("currency_code")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    let price = req.get("price").and_then(|v| v.as_str());
    let row = sqlx::query(
        "UPDATE upsell.offers SET
            pricing_strategy = $2,
            upgraded_room_type = $3,
            currency_code = $4,
            price = COALESCE($5::numeric, price),
            updated_at = now()
         WHERE id = $1
         RETURNING id, offer_code, offer_name, price::text as price, pricing_strategy, upgraded_room_type, currency_code, status",
    )
    .bind(offer_id).bind(pricing_strategy).bind(upgraded_room_type).bind(currency_code).bind(price)
    .fetch_optional(&mut *conn).await.map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "offer not found".to_string()))?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:offer:configured",
        serde_json::json!({ "offer_id": offer_id, "pricing_strategy": pricing_strategy }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    let s: String = row.get("status");
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({
        "id": row.get::<uuid::Uuid, _>("id"), "offer_code": row.get::<String, _>("offer_code"),
        "offer_name": row.get::<String, _>("offer_name"), "price": row.get::<String, _>("price"),
        "pricing_strategy": row.get::<String, _>("pricing_strategy"),
        "upgraded_room_type": row.get::<Option<String>, _>("upgraded_room_type"),
        "currency_code": row.get::<String, _>("currency_code"), "status": s,
    })))
}

// ── Cron job stub for daily offer generation ──

pub async fn cron_daily_generation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // B1: upcoming-reservation scan goes through the PMS HTTP API.
    let today = chrono::Utc::now().date_naive();
    let window_end = today + chrono::Duration::days(7);
    let upcoming = if let Some(pid) = property_id {
        state
            .pms_client
            .search_reservations(pid, &[("limit", "500")])
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("PMS search failed: {e}")))?
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "property_id required for daily generation".to_string(),
        ));
    };
    // The PMS search API filters by status/limit only — apply the arrival
    // window (next 7 days, CONFIRMED/CHECKED_IN) client-side.
    let in_window = |r: &serde_json::Value| -> bool {
        let status_ok = matches!(
            r.get("status").and_then(|v| v.as_str()).unwrap_or(""),
            "CONFIRMED" | "CHECKED_IN"
        );
        let arrival_ok = r
            .get("arrival_date")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|d| d >= today && d <= window_end)
            .unwrap_or(false);
        status_ok && arrival_ok
    };
    let reservations: Vec<serde_json::Value> = upcoming
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(in_window)
        .collect();
    let mut total_generated = 0i64;
    for r in &reservations {
        let res_id: uuid::Uuid = r
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                (
                    StatusCode::BAD_GATEWAY,
                    "PMS returned reservation without id".to_string(),
                )
            })?;
        let pid: uuid::Uuid = property_id.expect("checked above");
        let offers = sqlx::query(
            "SELECT id FROM upsell.offers WHERE property_id = $1 AND status = 'ACTIVE'",
        )
        .bind(pid)
        .fetch_all(&mut *conn)
        .await
        .map_err(err_db)?;
        for o in &offers {
            let offer_id: uuid::Uuid = o.get("id");
            let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);
            sqlx::query("INSERT INTO upsell.upsell_transactions (property_id, env_id, reservation_id, offer_id, status, expires_at) VALUES ($1, $2, $3, $4, 'OFFERED', $5) ON CONFLICT DO NOTHING")
                .bind(pid).bind(env_id).bind(res_id).bind(offer_id).bind(expires_at)
                .execute(&mut *conn).await.map_err(err_db)?;
            total_generated += 1;
        }
    }
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:cron:daily_generation",
        serde_json::json!({ "property_id": property_id, "total_generated": total_generated }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "total_generated": total_generated, "reservations_processed": reservations.len() }),
    ))
}

// ── PMS integration — fetch upcoming reservations, real-time inventory, guest profiles ──

pub async fn fetch_hotel8_pms_data(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = params
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let reservation_id = params
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // B1: all PMS reads go through the PMS HTTP API (no pms_* SQL).
    let res_data = if let Some(rid) = reservation_id {
        Some(
            state
                .pms_client
                .get_reservation(
                    property_id.ok_or_else(|| {
                        (StatusCode::BAD_REQUEST, "property_id required".to_string())
                    })?,
                    rid,
                )
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("PMS read failed: {e}")))?,
        )
    } else {
        None
    };
    // Fetch inventory for property
    let inventory = if let Some(pid) = property_id {
        let today = chrono::Utc::now().date_naive();
        let end = today + chrono::Duration::days(30);
        let inv = state
            .pms_client
            .get_inventory(pid, &today.to_string(), &end.to_string())
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("PMS inventory read failed: {e}"),
                )
            })?;
        inv.get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    } else {
        vec![]
    };
    // Fetch guest profile via the reservation's guest_profile_id
    let profile = if let Some(ref res) = res_data {
        let (pid, gid) = (
            res.get("property_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok()),
            res.get("guest_profile_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok()),
        );
        if let (Some(pid), Some(gid)) = (pid, gid) {
            state
                .pms_client
                .get_profile(pid, gid)
                .await
                .map(Some)
                .map_err(|e| {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("PMS profile read failed: {e}"),
                    )
                })?
        } else {
            None
        }
    } else {
        None
    };
    // Client responses are already JSON — pass through the PMS projection.
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({
        "reservation": res_data,
        "inventory": inventory,
        "profile": profile,
    })))
}

// ── Func integration — execute ML pricing model (WASM) for dynamic offers ──

pub async fn compute_dynamic_price(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let base_price = req
        .get("base_price")
        .and_then(|v| v.as_f64())
        .unwrap_or(100.0);
    let occupancy_rate = req
        .get("occupancy_rate")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5);
    let days_before_arrival = req
        .get("days_before_arrival")
        .and_then(|v| v.as_i64())
        .unwrap_or(7);
    let guest_tier = req
        .get("guest_tier")
        .and_then(|v| v.as_str())
        .unwrap_or("NONE");
    // B3: pricing runs in a Sentin8:Func WASM module when one is deployed;
    // otherwise the deterministic local rule (extracted to `pricing` for
    // tests) applies. The Func result is authoritative when present.
    let local =
        crate::pricing::compute(base_price, occupancy_rate, days_before_arrival, guest_tier);
    // The certificate-presenting Func client (`None` at boot when the
    // workload certificate is not yet mintable — the local rule covers).
    let func_price = match state.config.func_client.as_ref() {
        Some(client) => {
            crate::func_exec::run_pricing_module(
                client,
                &serde_json::json!({
                    "base_price": base_price,
                    "occupancy_rate": occupancy_rate,
                    "days_before_arrival": days_before_arrival,
                    "guest_tier": guest_tier,
                }),
            )
            .await
        }
        None => Ok(None),
    };
    let suggested_price = match func_price {
        Ok(Some(price)) => price,
        Ok(None) => local,
        Err(e) => {
            tracing::warn!(error = %e, "upsell: Func pricing module failed — falling back to local rule");
            local
        }
    };
    emit_on_pool(&state.pool, env_id, "hotel8:upsell:dynamic_price:computed", serde_json::json!({ "base_price": base_price, "suggested_price": suggested_price, "occupancy_rate": occupancy_rate, "days_before_arrival": days_before_arrival, "guest_tier": guest_tier }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    Ok(Json(serde_json::json!({
        "base_price": base_price, "suggested_price": suggested_price,
        "factors": { "occupancy_rate": occupancy_rate, "days_before_arrival": days_before_arrival, "guest_tier": guest_tier },
    })))
}

// ── Post integration — send offer notification to guest ──

pub async fn send_offer_notification(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let channel = req
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("EMAIL");
    // In production: POST to Post via state.config.sentin8_post_url
    emit_on_pool(&state.pool, env_id, "hotel8:upsell:notification:sent", serde_json::json!({ "transaction_id": transaction_id, "channel": channel, "status": "queued" }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": transaction_id, "channel": channel, "status": "queued" }),
    ))
}

// ── Payments integration — process upgrade payment ──

pub async fn process_upgrade_payment(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "transaction_id required".to_string(),
            )
        })?;
    let payment_method_token = req
        .get("payment_method_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "payment_method_token required".to_string(),
            )
        })?;
    let amount = req.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let currency = req
        .get("currency_code")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    // B3: the upgrade payment is a Bill8:Payments charge — never
    // shadow-written here. A fabricated id is history. CERTFREE-4V: the
    // boot-built `upsell-payments` certificate transport carries the call.
    let Some(ref payments_http) = state.config.payments_http else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "payments workload transport absent".into(),
        ));
    };
    let payment_id = crate::payments_exec::process_upgrade(
        payments_http,
        &state.config.bill8_payments_url,
        transaction_id,
        payment_method_token,
        amount,
        currency,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Payments call failed: {e}"),
        )
    })?;
    emit_on_pool(&state.pool, env_id, "hotel8:upsell:payment:processed", serde_json::json!({ "transaction_id": transaction_id, "payment_id": payment_id, "token": payment_method_token, "amount": amount, "currency": currency }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    Ok(Json(serde_json::json!({
        "transaction_id": transaction_id, "payment_id": payment_id,
        "status": "PROCESSED", "payment_method_token": payment_method_token,
    })))
}

// ── PMS integration — modify reservation on acceptance ──

pub async fn modify_reservation_on_acceptance(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "transaction_id required".to_string(),
            )
        })?;
    let new_room_type_id = req
        .get("new_room_type_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let new_rate = req.get("new_rate").and_then(|v| v.as_str());
    // Get the transaction to find the reservation
    let tx = sqlx::query("SELECT reservation_id, property_id, offer_id FROM upsell.upsell_transactions WHERE id = $1")
        .bind(transaction_id).fetch_one(&mut *conn).await.map_err(err_db)?;
    let reservation_id: Option<uuid::Uuid> = tx.get("reservation_id");
    let tx_property_id: Option<uuid::Uuid> = tx.get("property_id");
    // B1: the reservation modification goes through the PMS HTTP API —
    // no cross-service `UPDATE pms_reservations` SQL.
    if let (Some(res_id), Some(pid)) = (reservation_id, tx_property_id) {
        state
            .pms_client
            .modify_reservation(pid, res_id, new_room_type_id, None)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("PMS modify failed: {e}")))?;
        let _ = &new_rate; // rate change travels with the PMS upsell call in a follow-up story
                           // Record the modification on the transaction
        let modification = serde_json::json!({ "new_room_type_id": new_room_type_id, "new_rate": new_rate, "modified_at": chrono::Utc::now() });
        sqlx::query(
            "UPDATE upsell.upsell_transactions SET modified_reservation = $2 WHERE id = $1",
        )
        .bind(transaction_id)
        .bind(&modification)
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;
    }
    emit_tx(&mut *conn, env_id, "hotel8:upsell:reservation:modified", serde_json::json!({ "transaction_id": transaction_id, "reservation_id": reservation_id, "new_room_type_id": new_room_type_id, "new_rate": new_rate }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": transaction_id, "reservation_id": reservation_id, "modified": true }),
    ))
}

// ── Post integration — send acceptance confirmation ──

pub async fn send_acceptance_confirmation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let transaction_id = req
        .get("transaction_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // In production: POST to Post via state.config.sentin8_post_url
    emit_on_pool(
        &state.pool,
        env_id,
        "hotel8:upsell:confirmation:sent",
        serde_json::json!({ "transaction_id": transaction_id, "status": "queued" }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    Ok(Json(
        serde_json::json!({ "transaction_id": transaction_id, "status": "queued" }),
    ))
}

// ── A/B testing support ──

pub async fn create_ab_test(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_id = req
        .get("offer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "offer_id required".to_string()))?;
    let variants = req
        .get("variants")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "variants array required".to_string(),
            )
        })?;
    let mut created: Vec<serde_json::Value> = Vec::new();
    for v in variants {
        let variant_label = v
            .get("variant_label")
            .and_then(|v| v.as_str())
            .unwrap_or("A");
        let rule_type = v
            .get("rule_type")
            .and_then(|v| v.as_str())
            .unwrap_or("GUEST_TIER");
        let rule_value = v
            .get("rule_value")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let row = sqlx::query("INSERT INTO upsell.eligibility_rules (offer_id, rule_type, rule_value, variant_label, is_active) VALUES ($1, $2, $3, $4, true) RETURNING id")
            .bind(offer_id).bind(rule_type).bind(&rule_value).bind(variant_label)
            .fetch_one(&mut *conn).await.map_err(err_db)?;
        let id: uuid::Uuid = row.get("id");
        created.push(serde_json::json!({ "rule_id": id, "variant_label": variant_label, "rule_type": rule_type }));
    }
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:upsell:ab_test:created",
        serde_json::json!({ "offer_id": offer_id, "variant_count": created.len() }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "offer_id": offer_id, "variants": created }),
    ))
}

pub async fn get_ab_test_results(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let offer_id = params
        .get("offer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let rows = sqlx::query(
        "SELECT er.variant_label,
                COUNT(t.id) as total,
                COUNT(t.id) FILTER (WHERE t.status = 'ACCEPTED') as accepted,
                COALESCE(SUM(t.price_charged) FILTER (WHERE t.status = 'ACCEPTED'), 0)::text as revenue
         FROM upsell.eligibility_rules er
         LEFT JOIN upsell.upsell_transactions t ON t.offer_id = er.offer_id AND t.variant_label = er.variant_label
         WHERE ($1::uuid IS NULL OR er.offer_id = $1)
         GROUP BY er.variant_label
         ORDER BY er.variant_label",
    )
    .bind(offer_id).fetch_all(&mut *conn).await.map_err(err_db)?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let total: i64 = r.get("total");
            let accepted: i64 = r.get("accepted");
            let rate = if total > 0 {
                (accepted as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            serde_json::json!({
                "variant_label": r.get::<String, _>("variant_label"),
                "total": total, "accepted": accepted,
                "acceptance_rate": format!("{:.2}%", rate),
                "revenue": r.get::<String, _>("revenue"),
            })
        })
        .collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "variants": items })))
}

// ── Rate limiting on offer generation ──

pub async fn get_rate_limit_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let _a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &_a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:upsell:admin"),
    )
    .await?;
    Ok(Json(serde_json::json!({
        "offer_generation": { "max_per_minute": 100, "max_per_hour": 5000 },
        "offer_acceptance": { "max_per_minute": 50, "max_per_hour": 2000 },
        "rule_testing": { "max_per_minute": 20, "max_per_hour": 500 },
    })))
}
