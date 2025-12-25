# Comparison

> *How FORGE compares to alternatives*

---

## vs Convex

| Aspect | Convex | FORGE |
|--------|--------|-------|
| **Hosting** | Managed only | Self-hosted |
| **Database** | Proprietary | PostgreSQL |
| **Language** | TypeScript | Rust + TypeScript |
| **Real-time** | ✅ Automatic | ✅ Automatic |
| **Type Safety** | ✅ End-to-end | ✅ End-to-end |
| **Background Jobs** | ✅ Built-in | ✅ Built-in |
| **Pricing** | Usage-based | Infrastructure cost |
| **Data Ownership** | Convex servers | Your servers |
| **Customization** | Limited | Full control |

**Choose Convex if:** You want zero-ops managed service.
**Choose FORGE if:** You need self-hosting, PostgreSQL, or Rust.

---

## vs SpacetimeDB

| Aspect | SpacetimeDB | FORGE |
|--------|-------------|-------|
| **Focus** | Games, simulations | Business apps |
| **Database** | Custom (WASM) | PostgreSQL |
| **Latency** | Sub-millisecond | Milliseconds |
| **Language** | Rust (WASM) | Rust (native) |
| **Deployment** | Managed + self-host | Self-hosted |
| **Background Jobs** | Limited | ✅ Full system |
| **Observability** | Limited | ✅ Built-in |

**Choose SpacetimeDB if:** Building real-time games.
**Choose FORGE if:** Building business applications.

---

## vs Supabase

| Aspect | Supabase | FORGE |
|--------|----------|-------|
| **Database** | PostgreSQL | PostgreSQL |
| **Real-time** | ✅ Change streams | ✅ Query subscriptions |
| **Backend Logic** | Edge Functions | Rust functions |
| **Type Safety** | Generated from DB | Generated from schema |
| **Background Jobs** | ❌ External needed | ✅ Built-in |
| **Hosting** | Managed + self-host | Self-hosted |
| **Auth** | ✅ Built-in | ✅ JWT + OAuth |

**Choose Supabase if:** You want managed PostgreSQL with extras.
**Choose FORGE if:** You want a complete framework with jobs/crons.

---

## vs Traditional Stack

| Aspect | Traditional | FORGE |
|--------|-------------|-------|
| **Components** | 5-10 services | 1 binary + PostgreSQL |
| **Setup Time** | Days-weeks | Minutes |
| **Real-time** | Manual WebSocket | Automatic |
| **Jobs** | Redis + worker | Built-in |
| **Observability** | Prometheus + Grafana | Built-in |
| **Type Safety** | Manual sync | Automatic |
| **Learning Curve** | Each service | One framework |

**Choose Traditional if:** You have specific requirements or existing infra.
**Choose FORGE if:** You want rapid development with batteries included.

---

## Summary

```
                    Managed ◄─────────────────────────► Self-Hosted
                        │                                    │
                        │   Convex                           │
                        │   ●                                │
                        │                                    │
                        │          Supabase                  │
                        │          ●                         │
                        │                                    │
                        │                    SpacetimeDB     │
                        │                    ●               │
                        │                                    │
                        │                              FORGE │
                        │                              ●     │
                        │                                    │
                        │                                    │
    Simple ◄────────────┼────────────────────────────────────┼─► Complex
                        │                                    │
                        │                                    │
```

FORGE is positioned for developers who want:
- Full control (self-hosted)
- Batteries included (simple to start)
- PostgreSQL (battle-tested, familiar)
- Rust (performance, reliability)
