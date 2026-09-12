<script context="module">
  export const panelKind = "info";
</script>
<script>
  import Counter from "./Counter.svelte";
  export let title;
  function describe() { return title + " panel"; }
</script>

<section><h2>{describe()}</h2><Counter label="Clicks" /></section>
