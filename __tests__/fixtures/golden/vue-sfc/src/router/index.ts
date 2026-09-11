import { createRouter, createWebHistory } from "vue-router";
const router = createRouter({
  history: createWebHistory(),
  routes: [
    { name: "home", path: "/", component: () => import("@/views/Home.vue") },
    { name: "login", path: "/login", component: () => import("@/views/Login.vue") },
    { name: "profile", path: "/profile/:username", component: () => import("@/views/Profile.vue") },
  ],
});
export default router;
