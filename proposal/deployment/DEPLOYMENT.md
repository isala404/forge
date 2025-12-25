# Deployment Overview

> *From laptop to production*

---

## Deployment Modes

| Mode | Nodes | Use Case | Cost |
|------|-------|----------|------|
| **Development** | 1 (local) | Building features | Free |
| **Single Node** | 1 | Small apps, < 1000 users | ~$20/mo |
| **Docker Compose** | 2-5 | Medium apps, < 50k users | ~$100/mo |
| **Kubernetes** | 5+ | Large apps, scaling | Variable |

---

## Progressive Scaling Path

```
Day 1           Week 1          Month 2         Month 6
───────────────────────────────────────────────────────────────►

┌─────────┐    ┌─────────┐    ┌─────────────┐   ┌──────────────┐
│ forge   │    │ Docker  │    │ Compose     │   │ Kubernetes   │
│ dev     │ ─► │ single  │ ─► │ 3 nodes     │ ─►│ auto-scale   │
│         │    │ node    │    │             │   │              │
└─────────┘    └─────────┘    └─────────────┘   └──────────────┘

Same binary. Same config. Just add nodes.
```

---

## Quick Start

### Development

```bash
forge dev
# App: http://localhost:5173
# Dashboard: http://localhost:8080/_dashboard
```

### Production (Single Node)

```bash
# Build
forge build --release

# Run
DATABASE_URL=postgres://... ./forge serve
```

### Production (Docker)

```bash
docker run -e DATABASE_URL=postgres://... \
  -p 8080:8080 \
  your-app:latest
```

---

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | - | PostgreSQL connection |
| `FORGE_ENV` | No | `production` | Environment name |
| `FORGE_PORT` | No | `8080` | HTTP port |
| `FORGE_GRPC_PORT` | No | `9000` | Internal gRPC port |
| `FORGE_SECRET` | Yes (prod) | - | Encryption key |

---

## Health Checks

```bash
# Liveness
curl http://localhost:8080/health

# Readiness (checks DB)
curl http://localhost:8080/ready
```

---

## Related Documentation

- [Local Dev](LOCAL_DEV.md) — Development setup
- [Docker](DOCKER.md) — Container deployment
- [Kubernetes](KUBERNETES.md) — K8s patterns
