# Forge with Next.js

Read this only for Next.js applications.

`forgelib` contains a platform-specific native Node addon. Keep Forge calls in the Node.js runtime rather than Edge middleware/routes. With Next.js App Router, list `forgelib` in `serverExternalPackages` so the bundler loads the native addon from `node_modules`:

```ts
import type { NextConfig } from "next";

const config: NextConfig = {
  serverExternalPackages: ["forgelib"],
};

export default config;
```

Keep Forge initialization and DSN access in server-only modules. Framework behavior can change independently of Forge, so verify the installed Next.js configuration API when upgrading Next.js.
