import { redirect, error } from '@sveltejs/kit';
import { getForge } from '$lib/server/forge.js';

/** Resolve a short code: enqueue a click event, then 303 to the destination. */
export async function GET({ params }) {
  const forge = await getForge();
  const url = await forge.kvGet(`link:${params.code}`);
  if (!url) throw error(404, 'no such link');
  // Fire-and-forget analytics onto the queue; the worker counts it.
  await forge.queueEnqueue('clicks', JSON.stringify({ code: params.code }), 5);
  throw redirect(303, url);
}
