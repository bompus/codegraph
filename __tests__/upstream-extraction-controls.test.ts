import { describe, expect, it } from 'vitest';
import { tryKernelExtract } from '../src/extraction/kernel';

// The fork resolves JVM imports from native binding rows. Renaming an import
// changes its local name while retaining the declared target's name.
describe('upstream extraction controls', () => {
  it('a Kotlin alias binding keeps the imported declaration as its target', () => {
    const result = tryKernelExtract('Aliases.kt', `import app.internal.unsafeFlow as flow
import app.Format
import app.util.*
`, 'kotlin');
    expect(result).not.toBeNull();
    expect(result!.bindings?.filter((binding) => binding.kind === 'import')).toEqual([
      expect.objectContaining({ name: 'flow', targetSpec: 'app.internal.unsafeFlow', targetName: 'unsafeFlow' }),
      expect.objectContaining({ name: 'Format', targetSpec: 'app.Format', targetName: 'Format' }),
      expect.objectContaining({ name: '*', targetSpec: 'app.util.*', targetName: '*' }),
    ]);
  });

  it('Kotlin enum entry members and method locals are inside their binding scopes', () => {
    const result = tryKernelExtract('Op.kt', `enum class Op {
  PLUS {
    val scale: Int = 2
    override fun apply(at: Int): Int {
      val value = at * scale
      return value
    }
  };
  abstract fun apply(at: Int): Int
}
`, 'kotlin');
    expect(result).not.toBeNull();
    const member = result!.nodes.find((n) => n.kind === 'enum_member' && n.name === 'PLUS');
    const method = result!.nodes.find((n) => n.qualifiedName === 'Op::PLUS::apply');
    expect(member).toBeDefined();
    expect(method).toBeDefined();
    expect(member!.endLine).toBe(member!.startLine);
    const rows = result!.bindings ?? [];
    const declaredMethod = rows.find((b) => b.nodeId === method!.id);
    const scale = rows.find((b) => b.name === 'scale');
    const value = rows.find((b) => b.name === 'value');
    for (const binding of [declaredMethod, scale, value]) {
      expect(binding).toBeDefined();
      expect(binding!.scopeStart).toBeLessThanOrEqual(binding!.line);
      expect(binding!.scopeEnd).toBeGreaterThanOrEqual(binding!.line);
    }
    expect(declaredMethod!.scopeEnd).toBeGreaterThanOrEqual(method!.endLine);
  });

});
