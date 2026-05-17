import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    {
      type: 'doc',
      id: 'index',
      label: 'Overview',
    },
    {
      type: 'category',
      label: 'Start',
      collapsed: false,
      items: [
        'start/first-app',
        'start/anatomy',
      ],
    },
    {
      type: 'category',
      label: 'Tutorials',
      collapsed: false,
      items: [
        'tutorials/realtime-todo',
        'tutorials/authentication',
        'tutorials/background-processing',
        'tutorials/shipping-to-production',
      ],
    },
    {
      type: 'category',
      label: 'Build',
      collapsed: true,
      items: [
        'build/read-data',
        'build/subscribe-to-changes',
        'build/write-data',
        'build/file-uploads',
        'build/protect-routes',
        'build/background-work',
        'build/scheduled-tasks',
        'build/long-processes',
        'build/persistent-services',
        'build/webhooks',
        'build/expose-mcp-tools',
        'build/custom-handlers',
      ],
    },
    {
      type: 'category',
      label: 'Connect',
      collapsed: true,
      items: [
        'connect/generated-client',
        'connect/track-progress',
      ],
    },
    {
      type: 'category',
      label: 'Ship',
      collapsed: true,
      items: [
        'ship/configuration',
        'ship/production-architecture',
        'ship/signals',
        'ship/mcp-security',
        'ship/testing',
        'ship/security',
        'ship/deploy',
        'ship/migrations',
      ],
    },
    {
      type: 'category',
      label: 'Scale',
      collapsed: true,
      items: [
        'scale/performance',
        'scale/binary-size',
        'scale/multiple-nodes',
        'scale/worker-pools',
        'scale/reactivity',
        'scale/global-deploy',
        'scale/overnight-success',
      ],
    },
    {
      type: 'category',
      label: 'Agents',
      collapsed: true,
      items: [
        'agents/dev-loop',
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      collapsed: true,
      items: [
        'reference/cli',
        'reference/contexts',
        'reference/attributes',
        'reference/errors',
        'reference/pitfalls',
        'reference/wire-protocol',
        'reference/observability-catalog',
      ],
    },
  ],
};

export default sidebars;
