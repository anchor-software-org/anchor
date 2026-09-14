<script lang="ts">
  import { onMount } from 'svelte';

  export let value = '';
  export let options: Array<{ value: string; label: string; detail?: string }> = [];
  // A caller can provide the selected entry directly when its state snapshot
  // already identifies it. This avoids a second derived lookup for controls
  // whose selection must be reflected immediately after a native update.
  export let selected: { label: string; detail?: string } | undefined = undefined;
  export let disabled = false;
  export let ariaLabel = 'Select an option';
  export let onselect: (value: string) => void;

  let open = false;
  let root: HTMLDivElement;

  // Keep the collapsed trigger in sync when the parent replaces its snapshot
  // after a native state update. Calling a derived helper in the template did
  // not consistently invalidate this value across those prop updates.
  $: selectedOption = selected ?? options.find((option) => option.value === value) ?? options[0];

  const choose = (nextValue: string) => {
    open = false;
    if (nextValue !== value) onselect(nextValue);
  };

  const handleKeydown = (event: KeyboardEvent) => {
    if (event.key === 'Escape') open = false;
  };

  onMount(() => {
    const closeOutside = (event: PointerEvent) => {
      if (!root.contains(event.target as Node)) open = false;
    };
    document.addEventListener('pointerdown', closeOutside);
    return () => document.removeEventListener('pointerdown', closeOutside);
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="anchor-select" class:open bind:this={root}>
  <button
    type="button"
    class="select-trigger"
    aria-label={ariaLabel}
    aria-haspopup="listbox"
    aria-expanded={open}
    {disabled}
    onclick={() => open = !open}
  >
    <span class="select-value"><strong>{selectedOption?.label ?? ''}</strong>{#if selectedOption?.detail}<small>{selectedOption?.detail}</small>{/if}</span><i aria-hidden="true"></i>
  </button>
  {#if open}
    <div class="select-menu" role="listbox" aria-label={ariaLabel}>
      {#each options as option}
        <button
          type="button"
          role="option"
          aria-selected={option.value === value}
          class:selected={option.value === value}
          onclick={() => choose(option.value)}
        ><span class="select-value"><strong>{option.label}</strong>{#if option.detail}<small>{option.detail}</small>{/if}</span></button>
      {/each}
    </div>
  {/if}
</div>
