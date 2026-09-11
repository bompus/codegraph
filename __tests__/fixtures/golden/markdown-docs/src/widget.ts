/** Creates a widget. Documented in docs/architecture.md. */
export function createWidget(name: string): Widget {
  return { name, paint: () => paint(name) };
}
export interface Widget { name: string; paint(): void }
/** See README.md "Usage". */
export function render(w: Widget): void {
  w.paint();
}
function paint(name: string): void {
  console.log("painting", name);
}
