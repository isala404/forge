import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'FORGE',
  tagline: 'From Schema to Ship in a Single Day',
  favicon: 'img/favicon.ico',

  future: {
    v4: true,
  },

  url: 'https://forge.dev',
  baseUrl: '/',

  organizationName: 'forge',
  projectName: 'forge',

  onBrokenLinks: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/forge-social-card.jpg',
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'FORGE',
      logo: {
        alt: 'FORGE Logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          to: '/tutorials',
          label: 'Tutorials',
          position: 'left',
        },
        {
          to: '/api',
          label: 'API Reference',
          position: 'left',
        },
        {
          href: 'https://github.com/forge/forge',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Learn',
          items: [
            {
              label: 'Quick Start',
              to: '/quick-start',
            },
            {
              label: 'Tutorials',
              to: '/tutorials',
            },
            {
              label: 'Core Concepts',
              to: '/concepts/how-it-works',
            },
          ],
        },
        {
          title: 'Reference',
          items: [
            {
              label: 'API Reference',
              to: '/api',
            },
            {
              label: 'CLI Reference',
              to: '/cli',
            },
            {
              label: 'Background Processing',
              to: '/background',
            },
          ],
        },
        {
          title: 'Community',
          items: [
            {
              label: 'Discord',
              href: 'https://discord.gg/forge',
            },
            {
              label: 'GitHub',
              href: 'https://github.com/forge/forge',
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} FORGE. Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['rust', 'bash', 'typescript', 'sql', 'toml'],
    },
    algolia: undefined,
  } satisfies Preset.ThemeConfig,
};

export default config;
