export function canonical(value: unknown): string {
  function sorted(value: any): any {
    if (Array.isArray(value)) return value.map(sorted);
    if (value && typeof value === 'object') {
      return Object.fromEntries(Object.keys(value).sort().map((key) => [key, sorted(value[key])]));
    }
    return value;
  }
  return JSON.stringify(sorted(value)) + '\n';
}
