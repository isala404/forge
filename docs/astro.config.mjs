// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLlmsTxt from 'starlight-llms-txt';

// https://astro.build/config
export default defineConfig({
	// Production origin, used for canonical URLs and the sitemap. Pages serves this repo
	// from the tryforge.dev custom domain (see .github/workflows/deploy-docs.yml).
	site: 'https://tryforge.dev',
	// The custom domain serves from the root, so no base path is needed. Only set `base`
	// (e.g. '/forge') if you switch to a GitHub project page like isala404.github.io/forge.
	// base: '/forge',

	integrations: [
		starlight({
			// Serves /llms.txt (an index for coding agents) and /llms-full.txt (the whole
			// site as one Markdown file), generated from these same docs pages at build time.
			plugins: [
				starlightLlmsTxt({
					description:
						'The standard library for agent-built SaaS. Eight backend primitives (kv, queue, pub/sub, blob, auth, rate limit, schedule, config/flags) on one Postgres connection, with the same API in Rust, Node, and Python.',
					details:
						'Forge is one library that gives an app eight backend primitives on a single Postgres database, with a memory backend for tests that passes the same conformance suite. To generate correct Forge code, install the forge-idiomatic-developer skill (`npx skills add isala404/forge`); the API is not in model training data.',
					// A human-readable index of the individual docs pages, alongside the
					// generated full/abridged sets above.
					optionalLinks: [
						{ label: 'Quickstart', url: 'https://tryforge.dev/quickstart/', description: 'Install Forge, point it at Postgres, and make your first calls in Rust, Node, or Python.' },
						{ label: 'Primitives', url: 'https://tryforge.dev/primitives/', description: "The eight primitives, what each is for, and the methods you'll call." },
						{ label: 'Recipes', url: 'https://tryforge.dev/recipes/', description: 'End-to-end patterns: auth flows, background jobs, live updates, file handling, scheduled work.' },
						{ label: 'Configuration', url: 'https://tryforge.dev/configuration/', description: 'Every forge.toml setting, its default, and what it changes.' },
						{ label: 'Operations', url: 'https://tryforge.dev/operations/', description: 'Running Forge in production: system database, workers, scheduler, health, durability, errors.' },
						{ label: 'Reference', url: 'https://tryforge.dev/reference/', description: 'Cross-language method index, return types, the error taxonomy, and limits.' },
						{ label: 'Tutorial: to-do app (Rust)', url: 'https://tryforge.dev/tutorials/todo-app/', description: 'A Rust and Rocket API on auth, key/value, rate limit, and queue.' },
						{ label: 'Tutorial: URL shortener (Python)', url: 'https://tryforge.dev/tutorials/url-shortener/', description: 'A Python and FastAPI link shortener touching all eight primitives.' },
						{ label: 'Tutorial: chat app (Node)', url: 'https://tryforge.dev/tutorials/chat-app/', description: 'A Node and Hono realtime chat with presence, typing, and attachments.' },
					],
				}),
			],
			title: 'Forge',
			description:
				'The standard library for agent-built SaaS. Eight backend primitives, one Postgres connection, the same API in Rust, Node, and Python.',
			logo: {
				light: './src/assets/logo-light.svg',
				dark: './src/assets/logo-dark.svg',
				alt: 'Forge',
			},
			favicon: '/favicon.svg',
			customCss: ['./src/styles/forge.css'],
			components: {
				// Disable the prev/next pagination at the bottom of pages.
				Pagination: './src/components/Pagination.astro',
			},
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/isala404/forge' },
			],
			tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
			sidebar: [
				{ label: 'Quickstart', slug: 'quickstart' },
				{
					label: 'Tutorials',
					items: [
						{ slug: 'tutorials/todo-app' },
						{ slug: 'tutorials/url-shortener' },
						{ slug: 'tutorials/chat-app' },
					],
				},
				{
					label: 'Guide',
					items: [
						{ slug: 'primitives' },
						{ slug: 'recipes' },
						{ slug: 'configuration' },
						{ slug: 'operations' },
						{ slug: 'api-stability' },
					],
				},
				{ label: 'Reference', slug: 'reference' },
			],
		}),
	],
});
