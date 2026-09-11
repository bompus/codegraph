<template>
  <form @submit.prevent="submit"><input v-model="name" /><router-link :to="{ name: 'home' }">Back</router-link></form>
</template>
<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";
import { useSessionStore } from "@/stores/session";
const name = ref("");
const router = useRouter();
const session = useSessionStore();
async function submit() {
  session.login(name.value, "token");
  await router.push({ name: "home" });
}
</script>
