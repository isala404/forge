import { json, error } from '@sveltejs/kit';
import { getForge, base62 } from '$lib/server/forge.js';

/** List every link with its click count. */
export async function GET() {
  const forge = await getForge();
  const keys = await forge.kvScan('link:', 1000);
  const out = [];
  for (const key of keys) {
    const code = key.slice('link:'.length);
    const url = (await forge.kvGet(key)) ?? '';
    const clicks = await forge.kvIncr(`clicks:${code}`, 0); // INCRBY 0 reads the counter
    out.push({ code, url, clicks });
  }
  out.sort((a, b) => b.clicks - a.clicks || a.code.localeCompare(b.code));
  return json(out);
}

/** Create a short link. */
export async function POST({ request }) {
  const body = await request.json().catch(() => ({}));
  const url = String(body.url ?? '').trim();
  if (!/^https?:\/\//.test(url)) {
    throw error(400, 'url must start with http:// or https://');
  }
  const forge = await getForge();
  const seq = await forge.kvIncr('linkly:seq', 1); // atomic id sequence
  const code = base62(seq);
  await forge.kvSet(`link:${code}`, url);
  return json({ code, short_url: `/r/${code}` });
}
