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
      label: 'Build',
      collapsed: false,
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
        'ship/testing',
        'ship/deploy',
      ],
    },
    {
      type: 'category',
      label: 'Scale',
      collapsed: true,
      items: [
        'scale/multiple-nodes',
        'scale/worker-pools',
        'scale/global-deploy',
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
      ],
    },
  ],
};

export default sidebars;
