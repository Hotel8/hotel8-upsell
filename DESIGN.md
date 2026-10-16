# DESIGN.md: Hotel8:Upsell — Detailed Design Document

## 1. Scope & Context

**Owned by Upsell:** database `hotel8`, schema `upsell` — 5 tables
(`upsell.eligibility_rules`, `upsell.offers`, `upsell.upsell_transactions`,
+ producer tables `upsell.outbox` / `upsell.audit_journal`); event family
`upsell`; the offer-generation direct consumer; RFW admin pages.

**NOT owned:** guest identity/tier truth (Sentin8:Auth + PMS profile),
room inventory (hotel8:pms), money movement (Bill8:Payments via the
payments lane), notification transport (Sentin8:Post), scheduling
(Sentin8:Cron).

### System context

```mermaid
graph TD
    subgraph "hotel8 vertical"
        UPS[hotel8:upsell]
        PMS2[hotel8:pms] -- "hotel8:pms:reservation:created (consumer group upsell-offers)" --> UPS
        LOY[hotel8:loyalty] -. "member tier (HTTP)" .-> UPS
    end
    subgraph "platform"
        ING[Sentin8:Proxy] -- "X-Sentin8-* context" --> UPS
        BUS2[Sentin8:Bus] <-- "POST /v1/produce (M2M)" --> R2[shell relay]
        R2 --- UPS
        CRON[Sentin8:Cron] -- "daily generation trigger" --> UPS
        PAY2[Bill8:Payments] -. "charges (bill8:payments:charges:write)" .-> UPS
    end
    DB3[(db hotel8 / schema upsell)] --> UPS
```

### Container view

```mermaid
graph TD
    subgraph upselld["hotel8:upsell"]
        H3[/v1 rules offers admin/]
        GEN[eligibility + pricing engine]
        SM3[offer lifecycle states]
        OUT3[(upsell.outbox)]
        AJ3[(upsell.audit_journal)]
    end
    GEN --> DB4[(upsell.* tables)]
    GEN --> OUT3
    CONS[consumer task] --> GEN
```

## 2. Runtime Architecture

```mermaid
graph LR
    BOOT3[shell::serve] --> ENG3[Shield]
    BOOT3 --> RP3[(pool cap 3)]
    BOOT3 --> RL3[relay]
    BOOT3 --> PJ[audit pusher]
    CON3[consumer task] -- "SSE consume + ack" --> BUSX[(stream upsell)]
    EGEN[generation on reservation.created] --> OFFL[offer rows]
```

### Request lifecycle — offer generation (event path)

```mermaid
sequenceDiagram
    participant BUS as Sentin8:Bus (stream upsell)
    participant U as hotel8:upsell consumer
    participant DB as db hotel8 (upsell)
    BUS->>U: hotel8:pms:reservation:created
    U->>DB: set_tenant_context(payload.env_id)
    U->>DB: SELECT upsell.eligibility_rules (tenant-authored)
    U->>U: evaluate (stay window, room-type compat, tier, caps)
    alt eligible variants exist
        U->>DB: INSERT upsell.offers (A/B variant, dynamic price)
        U->>DB: INSERT upsell.outbox (hotel8:upsell:offers:generated)
        U->>BUS: ack
    else none
        U->>BUS: ack (no-op)
    end
```

### Request lifecycle — guest accepts (API path)

```mermaid
sequenceDiagram
    participant G as Guest app (eguest via BFF)
    participant U as hotel8:upsell
    participant S as Shield
    participant DB as db hotel8
    participant PAY as Bill8:Payments
    G->>U: POST /v1/offers/{id}/accept
    U->>S: require(hotel8:upsell:reservation:write)
    S-->>U: permit
    U->>DB: BEGIN, offer state → accepted, journal, outbox, COMMIT
    U->>PAY: M2M charge create (bill8:payments:charges:write)
    PAY-->>U: charge → hotel8:upsell:payment:processed
    U-->>G: 200 confirmation
```

## 3. Data Model

```mermaid
erDiagram
    upsell_eligibility_rules ||--o{ upsell_offers : generates
    upsell_offers ||--o| upsell_transactions : settles
```

- `upsell.eligibility_rules` — tenant-authored rule sets (env-scoped).
- `upsell.offers` — generated offers: booking ref, variant, dynamic price
  + provenance, lifecycle state, decline memory.
- `upsell.upsell_transactions` — acceptance/settlement records.

### Offer state machine

```mermaid
stateDiagram-v2
    [*] --> OFFERED: generated
    OFFERED --> ACCEPTED: guest accepts
    OFFERED --> DECLINED: guest declines (cap recorded)
    OFFERED --> EXPIRED: expiry
    ACCEPTED --> [*]: payment processed
    DECLINED --> [*]
```

## 4. API Surface

**Public** (guest-facing through BFF; `hotel8:upsell:{read,admin}` mirror):
list my offers, accept/decline offer, offer detail.

**Admin** (`hotel8:upsell:admin` + Shield admin triplet): eligibility rules
CRUD (`hotel8:upsell:rule:created`, `rule:test` preview), A/B test setup,
transaction browsing, offer configuration
(`hotel8:upsell:offer:configured`).

### Cross-service data access — decision order (membus → bus → owner API)

Offer generation is event-first: `hotel8:pms:reservation:created` (consumer
group `upsell-offers`) carries the booking snapshot in the payload — the hot
path needs **zero** PMS call-backs (payload completeness rule). MemBus:
none (no tick-level state to refresh; offers change at human pace).
Residual synchronous reads (stay-context enrichment for the guest offer
list: reservation, inventory, profile) use the PMS owner API with an upsell
M2M token — a read of public query surfaces, never a bespoke internal
endpoint. No pms-schema grant exists or is allowed.

**Event flow** — identical outbox → relay → `upsell` stream shape:

```mermaid
sequenceDiagram
    participant DB as upsell.outbox
    participant R as shell relay
    participant B as Sentin8:Bus
    DB->>R: NOTIFY + poll
    R->>B: M2M produce
    alt accepted
        B-->>R: 200 → row relayed
    else down
        DB->>DB: attempts++, DLQ at max
    end
```

## 5. Cross-Cutting Concerns

- Auth/tenant/RLS/audit/drain/TLS/pool — the platform contract, identical to
  PMS/POS construction; event-driven generation is the consumer-first path
  (no internal API exists for "generate on booking").
- Frequency caps and decline memory are enforcement, not presentation — the
  engine filters before pricing.

## 6. Failure Modes & Recovery

```mermaid
flowchart TD
    A[PMS event replay] --> B[idempotent by booking+rule pair — no duplicate offer]
    C[Payments decline] --> D[offer remains in ACCEPTED-with-unsettled? no — state records payment failure; reprocess task retries]
    E[Bus outage] --> F[outbox persists; relay poll retries]
    G[Rule authoring error] --> H[rule:test preview renders evaluation without persisting offers]
```

## 7. Concurrency & Performance Model

```mermaid
graph LR
    CON4[consumer] --> RP4[(pool cap 3)]
    API3[admin/guest API] --> RP4
    RL4[relay] --> BX[("bus stream upsell")]
    PJ[audit pusher] --> SYS3[("Sys /v1/audit")]
```

- Single runtime; the consumer task handles the PMS stream; pool cap 3.
- Generation cost is dominated by rule evaluation (in-memory) + one INSERT;
  the event payload carries the booking snapshot — no PMS call-back on the
  hot path.

## 8. Invariants, Insecure-Defaults Rejected, Unsafe Rationale

- One live offer per (booking, rule, variant-slot); declines are remembered
  per stay — a guest never sees the same declined offer class again.
- Payment state is only ever a *reflection* of payments results (never
  self-asserted).
- No `unsafe`; no platform-DB access; secrets never stored.
- Insecure defaults rejected: no offers generated for guests marked
  non-contactable (consent respected at the engine level).

## 9. Testing Strategy

```mermaid
graph TD
    U3[Unit — pricing + eligibility pure fns] --> I3[Integration — consumer wiring, policy parity]
    I3 --> E3[E2E — admin rule authoring via BFF]
```

- Unit: rule evaluation truth tables; price provenance; variant allocation.
- Integration: consumer idempotency under replay; policy mirror coverage.
- NOT here: Shield internals, Cron scheduling.

## 10. Open Questions

- Multi-armed allocation beyond A/B.
- Multi-touch stay-long journeys.

---

**Design attribution:** hotel8:upsell and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
