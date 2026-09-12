<script lang="ts">
  import { count, increment, formatCount } from "./store";
  export let label: string = "Counter";
  let doubled = 0;
  $: doubled = $count * 2;
  function handleClick(): void {
    increment();
  }
</script>

<button on:click={handleClick}>{label}: {formatCount($count)} (x2 = {doubled})</button>

<style>
  button { font-weight: bold; }
</style>
