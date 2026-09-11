<template>
  <table><tr v-for="row in rows" :key="row"><td :class="cellClass(row)">{{ row }}</td></tr></table>
</template>
<script setup lang="ts">
defineProps<{ rows: string[] }>();
function cellClass(row: string): string {
  return row.startsWith("!") ? "flagged" : "plain";
}
</script>
<style scoped>
.flagged { color: red; }
.plain { color: inherit; }
</style>
