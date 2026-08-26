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
						'A backend infrastructure library with eight bounded primitives and one contract across Rust, JavaScript on Node.js and Bun, Python, and Go.',
					details:
						'Forge runs inside an application backend. PostgreSQL provides shared durable state, memory provides process-local test state, and filesystem or S3-compatible storage may hold blob bytes. Applications own protocols, authorization policy, deployment, observability export, and every frontend choice.',
					// A human-readable index of the individual docs pages, alongside the
					// generated full/abridged sets above.
					optionalLinks: [
						{ label: 'Quickstart', url: 'https://tryforge.dev/quickstart/', description: 'Install Forge, choose a runtime profile, and make your first calls.' },
						{ label: 'Scope and costs', url: 'https://tryforge.dev/scope/', description: 'Hot-path costs, durability boundaries, partial adoption, and when to use a service directly.' },
						{ label: 'Primitive semantics', url: 'https://tryforge.dev/semantics/', description: 'Consistency, ordering, retry, outage, and multi-process guarantees for every primitive.' },
						{ label: 'Primitives', url: 'https://tryforge.dev/primitives/', description: "The eight primitives, what each is for, and the methods you'll call." },
						{ label: 'Recipes', url: 'https://tryforge.dev/recipes/', description: 'Backend patterns for auth, jobs, event transport, files, and scheduled work.' },
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
				'Eight bounded backend primitives with one contract across Rust, JavaScript, Python, and Go.',
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
						{ slug: 'support' },
						{ slug: 'scope' },
						{ slug: 'semantics' },
						{ slug: 'security' },
						{ slug: 'primitives' },
						{ slug: 'recipes' },
						{ slug: 'integrations' },
						{ slug: 'configuration' },
						{ slug: 'operations' },
						{ slug: 'performance' },
						{ slug: 'event-delivery' },
						{ slug: 'api-stability' },
					],
				},
				{ label: 'Reference', slug: 'reference' },
				{ label: 'Generated contract', slug: 'contract-reference-generated' },
			],
		}),
	],
});
