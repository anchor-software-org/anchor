<script lang="ts">
  export let name: 'previous' | 'play' | 'pause' | 'next';
</script>

<svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true" fill="currentColor">
  {#if name === 'previous'}
    <path d="M4 4h2v12H4zM16 4.6v10.8L7.5 10 16 4.6z" />
  {:else if name === 'next'}
    <path d="M14 4h2v12h-2zM4 4.6 12.5 10 4 15.4V4.6z" />
  {:else if name === 'pause'}
    <path d="M5 4h3.5v12H5zM11.5 4H15v12h-3.5z" />
  {:else}
    <path d="M6 3.8 15.5 10 6 16.2V3.8z" />
  {/if}
</svg>
