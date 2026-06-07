import { json } from '@sveltejs/kit';
import { getForge } from '$lib/server/forge.js';

/** Total clicks aggregated by the background worker. */
export async function GET() {
  const forge = await getForge();
  const total_clicks = await forge.kvIncr('stats:total_clicks', 0);
  return json({ total_clicks });
}
