import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    {
      type: 'doc',
      id: 'index',
      label: 'Introduction',
    },
    {
      type: 'doc',
      id: 'quick-start',
      label: 'Quick Start',
    },
    {
      type: 'doc',
      id: 'why-forge',
      label: 'Why FORGE?',
    },
    {
      type: 'category',
      label: 'Core Concepts',
      collapsed: false,
      items: [
        'concepts/how-it-works',
        'concepts/schema',
        'concepts/functions',
        'concepts/realtime',
      ],
    },
    {
      type: 'category',
      label: 'Tutorials',
      collapsed: false,
      items: [
        'tutorials/index',
        'tutorials/build-a-todo-app',
        'tutorials/user-authentication',
        'tutorials/background-jobs',
        'tutorials/realtime-updates',
      ],
    },
    {
      type: 'category',
      label: 'Background Processing',
      collapsed: true,
      items: [
        'background/index',
        'background/jobs',
        'background/crons',
        'background/workflows',
      ],
    },
    {
      type: 'category',
      label: 'Frontend',
      collapsed: true,
      items: [
        'frontend/index',
        'frontend/setup',
        'frontend/queries-mutations',
        'frontend/realtime-subscriptions',
        'frontend/job-tracking',
      ],
    },
    {
      type: 'category',
      label: 'API Reference',
      collapsed: true,
      items: [
        'api/index',
        'api/query-context',
        'api/mutation-context',
        'api/action-context',
        'api/job-context',
        'api/workflow-context',
        'api/forge-error',
      ],
    },
    {
      type: 'doc',
      id: 'cli/index',
      label: 'CLI Reference',
    },
  ],
};

export default sidebars;
