<template>
  <nav><h1>{{ title }}</h1><button @click="emit('go', 'news')">News</button><router-link to="/login">Login</router-link></nav>
</template>
<script setup lang="ts">
defineProps<{ title: string }>();
const emit = defineEmits<{ (e: "go", tag: string): void }>();
</script>
