<script lang="ts">
  let {
    label,
    value = $bindable(''),
    suffix = '',
    hint = '',
    error = '',
    inputmode = 'text',
    placeholder = '',
    type = 'text',
    onchange,
  }: {
    label: string;
    value?: string;
    suffix?: string;
    hint?: string;
    error?: string;
    inputmode?: 'text' | 'decimal' | 'numeric';
    placeholder?: string;
    type?: string;
    onchange?: () => void;
  } = $props();
  let el: HTMLInputElement;
</script>

<label class="block">
  <span class="mb-1 block text-sm text-muted">{label}</span>
  <span
    class="flex items-center rounded-xl border bg-bg px-3 focus-within:border-brand {error ? 'border-bad' : 'border-line'}"
  >
    <input
      bind:this={el}
      bind:value
      {type}
      {inputmode}
      {placeholder}
      autocomplete="off"
      class="num min-h-12 w-full bg-transparent text-fg outline-none placeholder:text-muted/60"
      onfocus={() => setTimeout(() => el?.scrollIntoView({ block: 'center', behavior: 'smooth' }), 250)}
      onchange={() => onchange?.()}
    />
    {#if suffix}<span class="ml-2 shrink-0 text-sm text-muted">{suffix}</span>{/if}
  </span>
  {#if error}<span class="mt-1 block text-sm text-bad">{error}</span>
  {:else if hint}<span class="mt-1 block text-sm text-muted">{hint}</span>{/if}
</label>
