import { sveltekit } from '@sveltejs/kit/vite';

/** @type {import('vite').UserConfig} */
const config = {
  plugins: [sveltekit()],
  // forge-node is a native .node addon; keep it out of Vite's bundle, load via Node require at runtime.
  ssr: { external: ['forge-node'] },
  optimizeDeps: { exclude: ['forge-node'] }
};

export default config;
