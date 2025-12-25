# Kubernetes Deployment

> *Production-grade scaling*

---

## Basic Deployment

```yaml
# forge-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge
spec:
  replicas: 3
  selector:
    matchLabels:
      app: forge
  template:
    metadata:
      labels:
        app: forge
    spec:
      containers:
      - name: forge
        image: my-app:latest
        ports:
        - containerPort: 8080
          name: http
        - containerPort: 9000
          name: grpc
        env:
        - name: DATABASE_URL
          valueFrom:
            secretKeyRef:
              name: forge-secrets
              key: database-url
        - name: FORGE_CLUSTER_DISCOVERY
          value: kubernetes
        - name: FORGE_CLUSTER_NAMESPACE
          valueFrom:
            fieldRef:
              fieldPath: metadata.namespace
        resources:
          requests:
            memory: "256Mi"
            cpu: "250m"
          limits:
            memory: "1Gi"
            cpu: "1000m"
        livenessProbe:
          httpGet:
            path: /health
            port: 8080
          initialDelaySeconds: 10
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /ready
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 5
---
# Headless service for cluster mesh
apiVersion: v1
kind: Service
metadata:
  name: forge-mesh
spec:
  clusterIP: None
  selector:
    app: forge
  ports:
  - port: 9000
    name: grpc
---
# Load balancer service
apiVersion: v1
kind: Service
metadata:
  name: forge
spec:
  type: LoadBalancer
  selector:
    app: forge
  ports:
  - port: 80
    targetPort: 8080
```

---

## Specialized Workers

```yaml
# General workers
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-workers-general
spec:
  replicas: 5
  template:
    spec:
      containers:
      - name: forge
        env:
        - name: FORGE_ROLES
          value: "worker"
        - name: FORGE_WORKER_CAPABILITIES
          value: "general"
---
# Media workers
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-workers-media
spec:
  replicas: 2
  template:
    spec:
      containers:
      - name: forge
        env:
        - name: FORGE_ROLES
          value: "worker"
        - name: FORGE_WORKER_CAPABILITIES
          value: "media"
        resources:
          requests:
            cpu: "2000m"
            memory: "4Gi"
```

---

## Auto-Scaling

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: forge-hpa
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: forge
  minReplicas: 3
  maxReplicas: 20
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
```

---

## RBAC for Kubernetes Discovery

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: forge
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: forge-endpoints-reader
rules:
- apiGroups: [""]
  resources: ["endpoints"]
  verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: forge-endpoints-reader
subjects:
- kind: ServiceAccount
  name: forge
roleRef:
  kind: Role
  name: forge-endpoints-reader
  apiGroup: rbac.authorization.k8s.io
```

---

## Related Documentation

- [Docker](DOCKER.md) — Container basics
- [Workers](../cluster/WORKERS.md) — Worker configuration
- [Clustering](../cluster/CLUSTERING.md) — How nodes form clusters
