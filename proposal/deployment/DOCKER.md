# Docker Deployment

> *Simple containerized deployment*

---

## Single Container

### Dockerfile

```dockerfile
# Build stage
FROM rust:1.75 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/my-app /usr/local/bin/forge
EXPOSE 8080 9000
CMD ["forge", "serve"]
```

### Run

```bash
docker build -t my-app .
docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://user:pass@host/db \
  -e FORGE_SECRET=your-secret-key \
  my-app
```

---

## Docker Compose (Recommended)

### Basic Setup

```yaml
# docker-compose.yml
version: '3.8'

services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: forge
      POSTGRES_USER: forge
      POSTGRES_PASSWORD: forge
    volumes:
      - postgres_data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U forge"]
      interval: 5s
      timeout: 5s
      retries: 5

  forge:
    image: my-app:latest
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      DATABASE_URL: postgres://forge:forge@postgres/forge
      FORGE_SECRET: ${FORGE_SECRET}
    ports:
      - "8080:8080"

volumes:
  postgres_data:
```

### Multi-Node Cluster

```yaml
# docker-compose.cluster.yml
version: '3.8'

services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: forge
      POSTGRES_USER: forge
      POSTGRES_PASSWORD: forge
    volumes:
      - postgres_data:/var/lib/postgresql/data

  forge:
    image: my-app:latest
    depends_on:
      - postgres
    environment:
      DATABASE_URL: postgres://forge:forge@postgres/forge
      FORGE_CLUSTER_DISCOVERY: dns
      FORGE_CLUSTER_DNS_NAME: forge
    deploy:
      replicas: 3
    ports:
      - "8080:8080"

  nginx:
    image: nginx:alpine
    ports:
      - "80:80"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf
    depends_on:
      - forge

volumes:
  postgres_data:
```

### nginx.conf

```nginx
upstream forge {
    server forge:8080;
}

server {
    listen 80;
    
    location / {
        proxy_pass http://forge;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
    }
}
```

---

## Deploy Commands

```bash
# Start
docker-compose up -d

# Scale
docker-compose up -d --scale forge=5

# View logs
docker-compose logs -f forge

# Stop
docker-compose down
```

---

## Health Checks

```yaml
forge:
  healthcheck:
    test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
    interval: 10s
    timeout: 5s
    retries: 3
    start_period: 10s
```

---

## Related Documentation

- [Deployment](DEPLOYMENT.md) — Overview
- [Kubernetes](KUBERNETES.md) — K8s deployment
- [Configuration](../reference/CONFIGURATION.md) — Environment variables
