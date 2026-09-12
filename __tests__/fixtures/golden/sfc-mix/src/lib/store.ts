import { writable } from "svelte/store";
export const count = writable(0);
export function increment(): void { count.update((n) => n + 1); }
export function formatCount(n: number): string { return `Count: ${n}`; }
