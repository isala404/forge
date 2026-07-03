const test = require('node:test');
const assert = require('node:assert/strict');

const { jsonCodec, forgeErrorCode, forgeErrorRetryable, runWorker, QueueJob } = require('../client.js');

test('jsonCodec round-trips objects', () => {
  const value = { a: 1, b: ['x', null], c: 'text' };
  assert.deepEqual(jsonCodec.decode(jsonCodec.encode(value)), value);
});

test('jsonCodec is strict JSON, matching the Python binding', () => {
  // A bare string is stored quoted, so json.loads on the Python side accepts it.
  assert.equal(jsonCodec.encode('hello'), '"hello"');
  assert.equal(jsonCodec.decode('"hello"'), 'hello');
  assert.deepEqual(jsonCodec.decode(Buffer.from('{"n":2}')), { n: 2 });
  assert.throws(() => jsonCodec.decode('not json'));
});

test('forgeErrorCode parses the message prefix, not error.code', () => {
  const e = new Error('NOT_FOUND: no such key');
  e.code = 'GenericFailure'; // what napi actually sets
  assert.equal(forgeErrorCode(e), 'NOT_FOUND');
  assert.equal(forgeErrorCode(new Error('UNAVAILABLE: pool timed out')), 'UNAVAILABLE');
  assert.equal(forgeErrorCode(new Error('plain failure')), undefined);
  assert.equal(forgeErrorCode(new Error('SOMETHING_ELSE: nope')), undefined);
  assert.equal(forgeErrorCode('CONFIG: bad toml'), 'CONFIG');
  assert.equal(forgeErrorCode(null), undefined);
});

test('forgeErrorRetryable is true only for UNAVAILABLE', () => {
  assert.equal(forgeErrorRetryable(new Error('UNAVAILABLE: down')), true);
  assert.equal(forgeErrorRetryable(new Error('BACKEND: broke')), false);
  assert.equal(forgeErrorRetryable(new Error('nope')), false);
});

function rawJob(payload, receipt = 'r1') {
  return {
    id: 'j1',
    receipt,
    payload: JSON.stringify(payload),
    attempt: 1,
    maxAttempts: 5,
    leasedUntilMs: Date.now() + 30000,
    queue: 'q',
  };
}

// A queue stub: serves the given raw jobs one per dequeue, then aborts the loop.
function stubClient(jobs, controller) {
  const calls = { acked: [], nacked: [], heartbeats: 0, dequeues: 0 };
  return {
    calls,
    async queueDequeue() {
      calls.dequeues += 1;
      if (jobs.length === 0) {
        controller.abort();
        return null;
      }
      return jobs.shift();
    },
    async queueAck(receipt) {
      calls.acked.push(receipt);
    },
    async queueNack(receipt, retrySeconds) {
      calls.nacked.push({ receipt, retrySeconds });
    },
    async queueHeartbeat() {
      calls.heartbeats += 1;
    },
  };
}

test('runWorker acks a handled job', async () => {
  const controller = new AbortController();
  const client = stubClient([rawJob({ n: 1 })], controller);
  const seen = [];
  await runWorker(client, 'q', async (job) => {
    assert.ok(job instanceof QueueJob);
    seen.push(job.payload);
  }, { signal: controller.signal, waitSeconds: 0 });
  assert.deepEqual(seen, [{ n: 1 }]);
  assert.deepEqual(client.calls.acked, ['r1']);
  assert.deepEqual(client.calls.nacked, []);
});

test('runWorker nacks and reports a failing handler', async () => {
  const controller = new AbortController();
  const client = stubClient([rawJob({ n: 1 })], controller);
  const reported = [];
  await runWorker(client, 'q', async () => {
    throw new Error('boom');
  }, {
    signal: controller.signal,
    retrySeconds: 7,
    onError: (error, job) => {
      reported.push({ message: error.message, receipt: job?.receipt });
    },
  });
  assert.deepEqual(client.calls.acked, []);
  assert.deepEqual(client.calls.nacked, [{ receipt: 'r1', retrySeconds: 7 }]);
  assert.deepEqual(reported, [{ message: 'boom', receipt: 'r1' }]);
});

test('runWorker nacks an undecodable payload', async () => {
  const controller = new AbortController();
  const raw = rawJob({});
  raw.payload = 'not json';
  const client = stubClient([raw], controller);
  const reported = [];
  await runWorker(client, 'q', async () => {
    assert.fail('handler must not run');
  }, {
    signal: controller.signal,
    onError: (error, job) => reported.push({ job }),
  });
  assert.equal(client.calls.nacked.length, 1);
  assert.deepEqual(reported, [{ job: undefined }]);
});

test('runWorker survives dequeue errors with backoff', async () => {
  const controller = new AbortController();
  let failures = 0;
  const reported = [];
  const client = {
    async queueDequeue() {
      if (failures < 2) {
        failures += 1;
        throw new Error('UNAVAILABLE: db blip');
      }
      controller.abort();
      return null;
    },
  };
  await runWorker(client, 'q', async () => {}, {
    signal: controller.signal,
    onError: (error) => reported.push(forgeErrorCode(error)),
  });
  assert.equal(failures, 2);
  assert.deepEqual(reported, ['UNAVAILABLE', 'UNAVAILABLE']);
});

test('runWorker heartbeats long-running handlers and honors a lost lease', async () => {
  const controller = new AbortController();
  const raw = rawJob({ n: 1 });
  let heartbeats = 0;
  const client = {
    async queueDequeue() {
      if (heartbeats === -1) throw new Error('done');
      if (raw.taken) {
        controller.abort();
        return null;
      }
      raw.taken = true;
      return raw;
    },
    async queueAck() {
      assert.fail('must not ack after the lease is lost');
    },
    async queueNack() {
      assert.fail('must not nack after the lease is lost');
    },
    async queueHeartbeat() {
      heartbeats += 1;
      throw new Error('PRECONDITION: unknown receipt: the lease was lost');
    },
  };
  let sawLeaseLost = false;
  await runWorker(client, 'q', async (job) => {
    // Outlive one heartbeat interval (1s floor at this visibility).
    await new Promise((resolve) => setTimeout(resolve, 1200));
    sawLeaseLost = job.leaseLost;
  }, { signal: controller.signal, visibilitySeconds: 0.03 });
  assert.ok(heartbeats >= 1);
  assert.equal(sawLeaseLost, true);
});
