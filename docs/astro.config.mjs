// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

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
					],
				},
				{ label: 'Reference', slug: 'reference' },
			],
		}),
	],
});
