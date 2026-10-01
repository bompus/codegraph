import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-csharp-extension-recursion-'));
  fs.writeFileSync(path.join(root, 'Values.cs'), `using System.Collections.Generic;
using System.Linq;
namespace Values;
public class BaseValue { public object InheritedLiteral() => this; }
public class Value : BaseValue {
  public object InstanceLiteral() => this;
  public object WrongArityLiteral(int count) => this;
}
public class OtherValue { }
public class SequenceValue : Value { public IReadOnlyList<Value> Elements { get; } }
public class OtherSequence : Value { public IReadOnlyList<OtherValue> Elements { get; } }
public class UnknownSequence : Value { public object Elements { get; } }
public static class Extensions {
  public static object LiteralValue(this Value value) {
    if (value is SequenceValue sequence)
      return sequence.Elements.Select(e => e.LiteralValue());
    return value;
  }
  public static object OtherLiteral(this Value value) {
    if (value is OtherSequence sequence)
      return sequence.Elements.Select(e => e.OtherLiteral());
    return value;
  }
  public static object UnknownLiteral(this Value value) {
    if (value is UnknownSequence sequence)
      return sequence.Elements.Select(e => e.UnknownLiteral());
    return value;
  }
  public static object InstanceLiteral(this Value value) {
    if (value is SequenceValue sequence)
      return sequence.Elements.Select(extensions => extensions.InstanceLiteral());
    return value;
  }
  public static object InheritedLiteral(this Value value) {
    if (value is SequenceValue sequence)
      return sequence.Elements.Select(extensions => extensions.InheritedLiteral());
    return value;
  }
  public static object WrongArityLiteral(this Value value) {
    if (value is SequenceValue sequence)
      return sequence.Elements.Select(extensions => extensions.WrongArityLiteral());
    return value;
  }
}
`);
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('C# extension recursion through collection lambda parameters', () => {
  const recurses = (name: string) => {
    const method = cg.getNodesInFile('Values.cs').find((node) => node.kind === 'method' && node.qualifiedName === `Values::Extensions::${name}`)!;
    return cg.getOutgoingEdgesFrom([method.id]).some((edge) => edge.kind === 'calls' && edge.target === method.id);
  };

  it('retains recursion when the declared element type matches the extension receiver', () => {
    expect(recurses('LiteralValue')).toBe(true);
  });

  it('refuses unrelated and unknown element types despite a unique method name', () => {
    expect(recurses('OtherLiteral')).toBe(false);
    expect(recurses('UnknownLiteral')).toBe(false);
  });

  it('respects applicable instance members, including inherited members', () => {
    expect(recurses('InstanceLiteral')).toBe(false);
    expect(recurses('InheritedLiteral')).toBe(false);
  });

  it('keeps the extension when the instance overload needs another argument', () => {
    expect(recurses('WrongArityLiteral')).toBe(true);
  });
});
