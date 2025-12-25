# Glossary

> *Terms and definitions*

---

## A

**Action**
A function type that can call external services. Not transactional.

**Advisory Lock**
PostgreSQL feature used for leader election. Session-level locks that auto-release on disconnect.

---

## C

**Capability**
A worker's declared ability to process certain job types (e.g., "media", "ml", "general").

**Cluster**
A group of FORGE nodes working together, sharing state via PostgreSQL.

**Cron**
A scheduled task that runs at specified intervals.

---

## D

**Dead Letter Queue (DLQ)**
Where failed jobs go after exhausting retries. Can be manually retried or discarded.

**Delta**
The difference between two query results, sent to subscribers instead of full results.

---

## F

**Function**
A unit of application logic. Types: Query, Mutation, Action.

---

## G

**Gateway**
Node role that handles HTTP/WebSocket connections from clients.

---

## J

**Job**
A background task that runs asynchronously. Durable and retryable.

---

## L

**Leader**
A singleton role in the cluster (e.g., scheduler). Only one node holds it at a time.

---

## M

**Mesh**
The gRPC network connecting all nodes for internal communication.

**Mutation**
A function type that modifies data. Runs in a transaction.

---

## N

**Node**
A single FORGE process. Can run multiple roles.

---

## Q

**Query**
A read-only function that can be cached and subscribed to.

---

## R

**Read Set**
The tables and rows a query accessed, used to determine subscription invalidation.

**Role**
A responsibility a node can take on: gateway, function, worker, scheduler.

---

## S

**Scheduler**
The leader role that assigns jobs to workers and triggers crons.

**SKIP LOCKED**
PostgreSQL feature for efficient job queue claiming without conflicts.

**Subscription**
A persistent query that pushes updates when results change.

---

## W

**Worker**
Node role that processes background jobs.

**Workflow**
A multi-step process with durable state and compensation support.
