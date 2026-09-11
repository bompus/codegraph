import { defineStore } from "pinia";
export const useSessionStore = defineStore("session", {
  state: () => ({ username: "", token: "" }),
  getters: { loggedIn: (s) => s.token.length > 0 },
  actions: {
    login(username: string, token: string) { this.username = username; this.token = token; },
    logout() { this.username = ""; this.token = ""; },
  },
});
