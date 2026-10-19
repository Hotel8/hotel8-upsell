# Hotel8:Upsell - Business Requirements & Functionality

> Audience: product owners, business analysts, and anyone evaluating what
> Hotel8:Upsell **does for the business**. For architecture read `README.md`;
> for wire-level detail read `DESIGN.md`.

## 1. Business Purpose

- Convert bookings into higher-value stays: upgrades, early check-in, late
  check-out, amenities.
- Make offer strategy tenant-editable (rules as data) with honest A/B
  measurement.
- Settle accepted upsells immediately through the payments lane.

## 2. Business Scope (What the Service Provides)

| Capability | Description |
|---|---|
| Eligibility rules | Tenant-authored conditions (stay window, room-type compatibility, guest tier, caps) with a test-preview |
| Offer generation | Event-driven on booking creation; daily re-eligibility sweep |
| Dynamic pricing | Contextual price with provenance (rule, inputs) |
| A/B testing | Variant allocation per offer for conversion comparison |
| Offer presentation | Guest-app offer listing + accept/decline |
| Settlement | Accepted paid offers settle through payments; transaction records reflect results |
| Frequency governance | Decline memory + frequency caps per stay |
| Admin console | RFW pages incl. shared Shield policy admin |

**What the service does NOT own (business boundary)**

| Concern | Owning service |
|---|---|
| Reservation changes (upgrade re-assignment) | hotel8:pms |
| Payment capture | Bill8:Payments |
| Room inventory truth | hotel8:pms |
| Loyalty balances/tiers | hotel8:loyalty |
| Notification delivery | Sentin8:Post |
| Guest identity/consent | Sentin8:Auth |

## 3. Business Capabilities

| Area | Business rule |
|---|---|
| Generation | An offer exists only for an eligible booking; eligibility is defined by the tenant's rule set, evaluated without developer involvement. |
| Pricing | The displayed price derives from the rule's pricing policy and the provenance is auditable. |
| Presentation | A guest never sees the same offer class again after declining it during the stay. |
| A/B testing | Guests are assigned to a variant deterministically; conversion compares like-for-like. |
| Acceptance | Accepting a paid offer records a transaction and settles; the offer cannot be double-charged. |
| Decline | Declining is recorded; no further generation for that class. |
| Expiry | Offers expire per stay timeline - stale offers cannot be accepted. |
| Modification coupling | Accepted upsells that change the stay are reflected back as a reservation modification event. |

## 4. Business Events (Outbound)

| Event | Fires when |
|---|---|
| `hotel8:upsell:offers:generated` | New offers created from eligibility evaluation |
| `hotel8:upsell:cron:daily_generation` | Daily sweep ran |
| `hotel8:upsell:offer:accepted / declined / updated / created / configured` | Offer lifecycle |
| `hotel8:upsell:offers:expired` | Offers passed their window |
| `hotel8:upsell:dynamic_price:computed` | A dynamic price was computed |
| `hotel8:upsell:payment:processed` | Payment settled (result reflected) |
| `hotel8:upsell:reservation:modified` | An accepted offer changed the stay |
| `hotel8:upsell:confirmation:sent`, `notification:sent` | Guest communication dispatched |
| `hotel8:upsell:ab_test:created`, `rule:created`, `rule:test` | Rule administration |

**Inbound (consumed):** `hotel8:pms:reservation:created` - triggers offer
generation.

## 5. Business Rules & Invariants

1. An offer is presented at most once per (stay, offer class) unless the
   tenant allows retargeting.
2. Guest consent gates presentation.
3. An accepted offer's price never changes after the guest's acceptance.
4. Payment state is only reflected from the payments lane - never asserted.
5. Rule changes affect future generation only; existing offers honor their
   recorded provenance.
6. Declined offers are remembered for the stay; guests cannot be re-shown.

## 6. Target-User Roles & Use Cases

| Persona | Use |
|---|---|
| Revenue/marketing manager | Author eligibility rules, set A/B tests, review conversion |
| Front office | Browse a booking's offers, accept on behalf of the guest at the desk |
| Guest | View and accept/decline offers in their app |
| Chain admin | Policy admin over the upsell surface |

## 7. KPIs / Success Criteria

- Offer conversion (accepted/offered) per variant - measurable and honest.
- Zero duplicate charges (payment idempotency holds).
- Zero guest complaints of repeated declined offers (cap memory works).
- Generation completes inside the booking-moment budget.

## 8. Dependencies & Reliance on Other Services

| Reliance | For |
|---|---|
| hotel8:pms | Booking facts (events), stay modifications |
| hotel8:loyalty | Tier-aware eligibility |
| Bill8:Payments | Settlement |
| Sentin8:Cron | Daily sweep trigger |
| Sentin8:Auth + Shield | Identity, ABAC (guest tier), authorization |
| Sentin8:Bus | `upsell` stream + PMS intake |
| Sentin8:Post | Offer notification delivery |

## 9. Acceptance Criteria

1. A new PMS booking produces eligible offers without any operator action.
2. A tenant can author, preview (`rule:test`), and enable a rule with no
   code change.
3. An accepted paid offer settles exactly once and records the transaction.
4. A declined offer never reappears for the same stay.
5. A/B variants measure conversion truthfully (assignment is recorded).

## 10. Open Business Questions

- Bandit-style allocation vs fixed A/B.
- Multi-touch upsell journeys across the stay.

---

**Design attribution:** hotel8:upsell and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
