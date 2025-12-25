# Architecture Decision Records

> *Why we made the choices we made*

---

## ADR-001: PostgreSQL as Only External Dependency

**Status:** Accepted

**Context:**
Modern applications typically require multiple services: database, cache, message queue, job queue, metrics store, etc. This creates operational complexity.

**Decision:**
Use PostgreSQL for everything: data, jobs, events, metrics, logs, traces, sessions.

**Rationale:**
- PostgreSQL is battle-tested (30+ years)
- ACID guarantees simplify reasoning
- One thing to backup, monitor, scale
- Features like LISTEN/NOTIFY, advisory locks, JSONB cover most needs
- Scales to millions of rows with proper indexing

**Tradeoffs:**
- Not optimal for very high-throughput queues (>100k jobs/sec)
- Time-series could be more efficient in specialized DB
- Acceptable for 99% of applications

---

## ADR-002: Single Binary Architecture

**Status:** Accepted

**Context:**
Microservices offer independent scaling but create operational overhead.

**Decision:**
Ship a single binary that contains all functionality. Roles are enabled via configuration.

**Rationale:**
- Inspired by CockroachDB, Consul, Vault
- Same binary from development to production
- No version skew between services
- Simpler deployment and debugging
- Can still scale horizontally by adding nodes

**Tradeoffs:**
- Larger binary size
- Can't use different languages for different services
- All code updates together

---

## ADR-003: PostgreSQL Advisory Locks for Leader Election

**Status:** Accepted

**Context:**
Need exactly one scheduler leader in the cluster.

**Decision:**
Use `pg_advisory_lock()` for leader election instead of Raft/Paxos.

**Rationale:**
- PostgreSQL already required
- Simple to implement and understand
- Automatic release on connection loss
- No additional consensus protocol to maintain

**Tradeoffs:**
- Depends on PostgreSQL availability
- Slightly slower failover than dedicated consensus (5-15 seconds)
- Acceptable for scheduler leader (not on critical path)

---

## ADR-004: SKIP LOCKED for Job Queue

**Status:** Accepted

**Context:**
Need distributed job claiming without double-processing.

**Decision:**
Use PostgreSQL `FOR UPDATE SKIP LOCKED` pattern.

**Rationale:**
- No additional infrastructure (Redis, RabbitMQ)
- ACID guarantees on job state
- Efficient with proper indexing
- Built-in visibility (SQL queries)

**Tradeoffs:**
- Lower throughput than specialized queues
- Polling required (mitigated by NOTIFY)
- Sufficient for <10k jobs/second

---

## ADR-005: Schema-Driven Code Generation

**Status:** Accepted

**Context:**
Need type safety from database to frontend.

**Decision:**
Define models in Rust, generate TypeScript types and Svelte stores.

**Rationale:**
- Single source of truth
- Compile-time type checking
- No manual type synchronization
- Changes propagate automatically

**Tradeoffs:**
- Requires code generation step
- Generated code must not be edited
- Learning curve for schema DSL

---

## ADR-006: Built-in Observability

**Status:** Accepted

**Context:**
Observability is essential but typically requires multiple services.

**Decision:**
Include metrics, logs, traces, and dashboard in the binary, stored in PostgreSQL.

**Rationale:**
- Zero additional setup
- Works from day one
- Single pane of glass
- Optional export to external tools

**Tradeoffs:**
- PostgreSQL storage less efficient than specialized stores
- Dashboard simpler than Grafana
- Sufficient for most applications

---

## ADR-007: Svelte 5 for Frontend

**Status:** Accepted

**Context:**
Need a reactive frontend framework for real-time updates.

**Decision:**
Use Svelte 5 with runes for fine-grained reactivity.

**Rationale:**
- Compile-time optimization (smaller bundles)
- Fine-grained reactivity (efficient updates)
- Less boilerplate than React
- First-class TypeScript support

**Tradeoffs:**
- Smaller ecosystem than React
- Newer, less battle-tested
- Good fit for FORGE's real-time focus

---

## ADR-008: gRPC for Inter-Node Communication

**Status:** Accepted

**Context:**
Nodes need to communicate for request forwarding and coordination.

**Decision:**
Use gRPC with Protocol Buffers for the mesh network.

**Rationale:**
- Efficient binary protocol
- Strong typing with protobuf
- Bidirectional streaming
- Good Rust support (tonic)

**Tradeoffs:**
- More complex than HTTP/JSON
- Debugging requires special tools
- Worth it for performance and type safety
