// Bootstrap the Forge client + background worker once at server startup.
import { getForge } from '$lib/server/forge.js';

getForge().catch((err) => console.error('[forge init]', err));
