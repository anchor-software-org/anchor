<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';

  type ResizeDirection = Parameters<ReturnType<typeof getCurrentWindow>['startResizeDragging']>[0];

  export let title: string;
  export let onerror: (error: unknown) => void;

  let maximized = false;
  const edges: ResizeDirection[] = ['North', 'South', 'East', 'West', 'NorthEast', 'NorthWest', 'SouthWest'];
  const run = (action: () => Promise<unknown>) => { void action().catch(onerror); };
  const syncMaximized = async () => { maximized = await getCurrentWindow().isMaximized(); };

  onMount(() => {
    let stopped = false;
    let unlisten: (() => void) | undefined;
    run(async () => {
      const stop = await getCurrentWindow().onResized(() => run(syncMaximized));
      if (stopped) stop();
      else {
        unlisten = stop;
        await syncMaximized();
      }
    });
    return () => { stopped = true; unlisten?.(); };
  });

  const toggleMaximize = () => run(async () => {
    await getCurrentWindow().toggleMaximize();
    await syncMaximized();
  });

  const resize = (event: MouseEvent, direction: ResizeDirection) => {
    if (event.button !== 0 || maximized) return;
    event.preventDefault();
    run(() => getCurrentWindow().startResizeDragging(direction));
  };
</script>

<header class="topbar">
  <div class="window-drag-region" data-tauri-drag-region>
    <h1 data-tauri-drag-region>{title}</h1>
  </div>
  <div class="window-controls" aria-label="Window controls">
    <button aria-label="Minimize window" title="Minimize" onclick={() => run(() => getCurrentWindow().minimize())}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 11h10" /></svg>
    </button>
    <button aria-label={maximized ? 'Restore window' : 'Maximize window'} title={maximized ? 'Restore' : 'Maximize'} onclick={toggleMaximize}>
      <svg viewBox="0 0 16 16" aria-hidden="true">
        {#if maximized}<path d="M5 5V3h8v8h-2M3 5h8v8H3z" />{:else}<path d="M3 3h10v10H3z" />{/if}
      </svg>
    </button>
    <button class="window-close" aria-label="Close window" title="Close" onclick={() => run(() => getCurrentWindow().close())}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m3 3 10 10M13 3 3 13" /></svg>
    </button>
  </div>
</header>

{#if !maximized}
  {#each edges as edge}
    <button class="window-resize-edge" data-edge={edge} aria-label={`Resize window ${edge}`} tabindex="-1" onmousedown={(event) => resize(event, edge)}></button>
  {/each}
  <button class="window-resize-grip" aria-label="Resize window" title="Drag to resize window" tabindex="-1" onmousedown={(event) => resize(event, 'SouthEast')}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 19 14-14M11 19l8-8M17 19l2-2" /></svg>
  </button>
{/if}

<style>
  .topbar { padding-right: 0; }
  .window-drag-region { display: flex; height: 100%; min-width: 0; flex: 1; align-items: center; user-select: none; cursor: default; }
  .window-controls { display: flex; height: 100%; }
  .window-controls button { display: grid; width: 44px; place-items: center; padding: 0; border: 0; color: var(--splash); background: transparent; }
  .window-controls button:hover { background: var(--water-mid); }
  .window-controls .window-close:hover { background: #a52b35; }
  button:focus-visible { outline: 2px solid var(--sky); outline-offset: -3px; }
  svg { display: block; width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.5; }
  .window-resize-edge { position: fixed; z-index: 40; padding: 0; border: 0; background: transparent; }
  [data-edge="North"], [data-edge="South"] { right: 10px; left: 10px; height: 5px; cursor: ns-resize; }
  [data-edge="North"] { top: 0; }
  [data-edge="South"] { bottom: 0; right: 28px; }
  [data-edge="East"], [data-edge="West"] { top: 10px; bottom: 10px; width: 5px; cursor: ew-resize; }
  [data-edge="East"] { right: 0; bottom: 28px; }
  [data-edge="West"] { left: 0; }
  [data-edge="NorthEast"], [data-edge="NorthWest"], [data-edge="SouthWest"] { width: 10px; height: 10px; }
  [data-edge="NorthEast"] { top: 0; right: 0; cursor: nesw-resize; }
  [data-edge="NorthWest"] { top: 0; left: 0; cursor: nwse-resize; }
  [data-edge="SouthWest"] { bottom: 0; left: 0; cursor: nesw-resize; }
  .window-resize-grip { position: fixed; z-index: 40; right: 0; bottom: 0; display: grid; width: 28px; height: 28px; place-items: center; padding: 0; border: 1px solid var(--rock); border-right: 0; border-bottom: 0; color: var(--splash); background: var(--ink); cursor: nwse-resize; }
  .window-resize-grip svg { width: 24px; height: 24px; stroke-width: 2; }
  .window-resize-grip:hover { background: var(--water-mid); }
</style>
