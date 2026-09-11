<template>
  <div class="home"><TheHeader :title="title" @go="goTo" /><PlayerTable :rows="rows" /></div>
</template>
<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import TheHeader from "@/components/TheHeader.vue";
import PlayerTable from "@/components/PlayerTable.vue";
import { useSessionStore } from "@/stores/session";
const router = useRouter();
const session = useSessionStore();
const rows = ref<string[]>([]);
const title = computed(() => (session.loggedIn ? `Hi ${session.username}` : "Welcome"));
function goTo(tag: string) {
  router.push({ path: "/", query: { tag } });
}
</script>
<style scoped>
.home { padding: 1rem; }
</style>
