<template>
  <div>Profile of {{ username }}</div>
</template>
<script>
export default {
  name: "Profile",
  props: { username: { type: String, required: true } },
  methods: {
    greet() { return "hello " + this.username; },
  },
};
</script>
