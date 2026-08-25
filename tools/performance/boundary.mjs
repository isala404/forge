import { performance } from 'node:perf_hooks';
import { writeFileSync } from 'node:fs';
import { encodeCloudEvent, decodeCloudEvent } from '../../bindings/node/client.js';

const args = process.argv.slice(2);
const valueAfter = (name, fallback) => {
  const index = args.indexOf(name);
  return index === -1 ? fallback : args[index + 1];
};
const iterations = Number(valueAfter('--iterations', '1000'));
const output = valueAfter('--output', null);
if (!Number.isSafeInteger(iterations) || iterations < 1) throw new TypeError('iterations must be a positive integer');
const event = { id: 'benchmark', source: 'urn:forge:performance', type: 'forge.benchmark', data: Buffer.from('boundary') };
const samples = [];
for (let iteration = 0; iteration < iterations; iteration += 1) {
  const started = performance.now();
  decodeCloudEvent(encodeCloudEvent(event));
  samples.push(performance.now() - started);
}
samples.sort((left, right) => left - right);
const rank = Math.max(0, Math.ceil(samples.length * 0.95) - 1);
const report = {
  schema_version: 1,
  kind: 'language_boundary',
  language: typeof Bun === 'undefined' ? 'node' : 'bun',
  iterations,
  metrics: [{ name: 'cloudevent_roundtrip_p95_ms', value: samples[rank], unit: 'ms' }],
};
const json = `${JSON.stringify(report, null, 2)}\n`;
if (output) writeFileSync(output, json);
else process.stdout.write(json);
