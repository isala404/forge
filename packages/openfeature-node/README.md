# `@forgelib/openfeature-provider`

Official OpenFeature server provider for Forge 1.1 flags. It supports boolean, string, number, and object values with stable variants and standard resolution reasons.

```js
const { OpenFeature } = require('@openfeature/server-sdk');
const { ForgeProvider, telemetryHook } = require('@forgelib/openfeature-provider');
const { ForgeClient } = require('forgelib');

const forge = await ForgeClient.init();
await OpenFeature.setProviderAndWait(new ForgeProvider(forge));
const client = OpenFeature.getClient();
client.addHooks(telemetryHook());

const details = await client.getStringDetails('checkout-theme', 'classic', {
  targetingKey: user.id,
  plan: user.plan,
});
```

The provider registers no hooks and mutates no global OpenFeature state. The application chooses provider and hook scope. `telemetryHook()` constructs the official OpenFeature OpenTelemetry `EventHook`, which emits `feature_flag.evaluation` span events using current semantic conventions. Analytics and flag-context privacy remain application responsibilities.
