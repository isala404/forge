"""A guided tour of every Forge primitive from Python, via the forge-py binding.

Mirrors examples/full_tour.rs: signup, session + API key, a feature flag, a rate
limit, a stored+presigned file, and a one-shot scheduled job — all asserted.

Run it:
    1. Start Postgres (repo root):  docker compose up -d db
    2. Build + install the binding into a venv:
         cd bindings/forge-py && python -m venv .venv && . .venv/bin/activate
         pip install maturin && maturin develop
    3. Run:  cd ../../examples/python && python full_tour.py
    (set FORGE_POSTGRES_URL if your Postgres isn't the docker-compose default)
"""

import os
import secrets
import time

from forge_py import ForgeClient

PG = os.environ.get(
    "FORGE_POSTGRES_URL", "postgres://postgres:forge@localhost:5432/forge_dev"
)
run = secrets.token_hex(6)
user_id = f"user:{run}"

forge = ForgeClient.connect(PG, "tour-secret-change-me")

# ---- auth: password, session, API key --------------------------------------
h = forge.hash_password("hunter2-correct-horse")
assert forge.verify_password("hunter2-correct-horse", h) is True
assert forge.verify_password("wrong", h) is False

token = forge.create_session(user_id)
assert forge.validate_session(token) == user_id

key_id, secret = forge.create_api_key(user_id, "cli")
assert secret.startswith("fk_")
assert forge.verify_api_key(secret) == user_id
print(f"auth: password verified, session + API key ({key_id}) minted")

# ---- config + flags --------------------------------------------------------
forge.config_set(f"plan:{run}", "pro")
assert forge.config_get(f"plan:{run}") == "pro"

flag = f"new_ui:{run}"
forge.set_flag_percent(flag, 100)
on = forge.flag(flag, False, user_id)
assert on is True
print(f"config: plan=pro stored; flag {flag} resolved to {on}")

# ---- ratelimit: 3 per minute, the 4th throttled ----------------------------
allowed = sum(1 for _ in range(4) if forge.rate_limit_check("login", user_id, 3, 60)[0])
assert allowed == 3
print(f"ratelimit: {allowed}/4 login attempts admitted (limit 3/min)")

# ---- blob: store, read back, presign ---------------------------------------
key = f"exports/{run}/report.csv"
forge.blob_put(key, "hello,world\n1,2\n", "text/csv")
assert forge.blob_get(key) == "hello,world\n1,2\n"
url = forge.blob_presign_download(key, 300)
print(f"blob: stored + presigned {url}")

# ---- schedule: a one-shot due now, fired into the queue --------------------
queue = f"reports_{run}"
job_id = forge.schedule_at(time.time(), queue, "generate-report")
fired = forge.run_scheduler_once()
assert fired >= 1
job = forge.queue_dequeue(queue, 30, 0)
assert job is not None and job[0] == job_id
forge.queue_ack(job[0])
print(f"schedule: one-shot {job_id} fired and consumed from {queue}")

# ---- kv: a counter ---------------------------------------------------------
assert forge.kv_incr(f"hits:{run}", 1) == 1

print("\nOK — every primitive worked end to end (via the Python binding).")
