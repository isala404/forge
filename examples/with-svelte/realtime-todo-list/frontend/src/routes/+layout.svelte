<script lang="ts">
  import { ForgeProvider } from "@forge-rs/svelte";
  import { PUBLIC_API_URL } from "$env/static/public";
  import { getToken, auth } from "$lib/forge/auth.svelte";
  import { onMount } from "svelte";

  interface Props {
    children: import("svelte").Snippet;
  }

  let { children }: Props = $props();

  onMount(() => {
    auth.startRefreshLoop(PUBLIC_API_URL);
  });
</script>

<ForgeProvider url={PUBLIC_API_URL} {getToken} onAuthError={() => auth.handleAuthError()}>
  {@render children()}
</ForgeProvider>
