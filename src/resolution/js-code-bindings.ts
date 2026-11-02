/**
 * Whether a JS/TS function binds a name in its own scope — a `const` / `let` /
 * `var` declaration or one of its parameters (name-matcher.ts's
 * `jsCodeBindsName`, kept here because the fork's only resolver is the Rust
 * kernel). The Tauri synthesizer uses it to tell a frontend's own `invoke` or
 * listener from the imported one at the call site (#3129).
 */
import type { Node } from '../types';
import type { ResolutionContext } from './types';
import { stripCommentsForRegex } from './strip-comments';

const PARAM_PAREN = '\\((?:[^()]|\\([^()]*\\))*\\)';
const PARAM_BRACE = '\\{(?:[^{}]|\\{[^{}]*\\})*\\}';
const PARAM_BRACKET = '\\[(?:[^\\[\\]]|\\[[^\\[\\]]*\\])*\\]';
const PARAM_ANGLE = '<(?![\\s])(?:=>|=(?!>)|[^<>=]|<(?:=>|=(?!>)|[^<>=])*>)*>';
const PARAM_PART = `(?:=>|=(?!>)|[^,(){}\\[\\]<>=]|<(?=\\s)|>(?=[\\s=])|${PARAM_PAREN}|${PARAM_BRACE}|${PARAM_BRACKET}|${PARAM_ANGLE})`;
const PARAM_MODIFIERS = '(?:(?:public|private|protected|readonly|override)\\s+)*';

interface LocalBindingPatterns {
  decl: RegExp;
  param: RegExp;
}

function localBindingPatterns(name: string, flags: string): LocalBindingPatterns {
  const n = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return {
    decl: new RegExp(
      '\\b(?:const|let|var)\\s+(?:' + n + '(?![\\w$])|[{\\[][^;=]*?\\b' + n + '(?![\\w$])[^;=]*?[}\\]])\\s*(?:=\\s*([^;\\n]*))?',
      flags
    ),
    param: new RegExp(
      '\\(\\s*(?:(?:' + PARAM_MODIFIERS + '(?:\\.\\.\\.)?[\\w$]+|' + PARAM_BRACE + '|' + PARAM_BRACKET + ')' +
        '(?:\\s*(?:\\?\\s*)?:' + PARAM_PART + '+|\\s*=' + PARAM_PART + '+|\\s*),\\s*)*' +
        PARAM_MODIFIERS + n + '(?![\\w$])(?:\\s*\\??\\s*:' + PARAM_PART + '*)?(?:\\s*=' + PARAM_PART + '*)?' +
        '(?:\\s*,(?:[^()]|' + PARAM_PAREN + ')*)?\\)\\s*(?::[^=;{]*)?(?:=>|\\{)',
      flags
    ),
  };
}

/** A parameter list that names `name`, never a control-flow head. */
function jsParameterListPattern(name: string, flags: string): RegExp {
  const { param } = localBindingPatterns(name, flags);
  const returnType = `(?::(?:[^=;{}()\\n]|${PARAM_BRACE})*)?`;
  return new RegExp(
    `(?=\\()(?<!\\b(?:if|while|for|switch|with)\\s*)${param.source.replace('(?::[^=;{]*)?', () => returnType)}`,
    flags
  );
}

interface FnCode {
  code: string;
  lineEnds: number[];
}
const JS_FN_CODE = new WeakMap<ResolutionContext, Map<string, FnCode>>();
const JS_FN_CODE_KEEP = 64;

/** The function's source through `line`, comments blanked. */
function jsFunctionCodeThrough(fn: Node, filePath: string, line: number, context: ResolutionContext): string {
  let fns = JS_FN_CODE.get(context);
  if (!fns) JS_FN_CODE.set(context, (fns = new Map()));
  const key = `${filePath}\0${fn.startLine}\0${fn.endLine}`;
  let own = fns.get(key);
  if (own) {
    fns.delete(key);
  } else {
    const lines = (context.getFileLines?.(filePath) ?? context.readFile(filePath)?.split(/\r?\n/) ?? [])
      .slice(fn.startLine - 1, fn.endLine);
    let end = -1;
    own = {
      code: stripCommentsForRegex(lines.join('\n'), 'javascript'),
      lineEnds: lines.map((text) => (end += text.length + 1)),
    };
    if (fns.size >= JS_FN_CODE_KEEP) fns.delete(fns.keys().next().value!);
  }
  fns.set(key, own);
  // Through `line`, or the file's last when it ends sooner.
  const last = Math.min(line, fn.startLine - 1 + own.lineEnds.length) - fn.startLine;
  return last < 0 ? '' : own.code.slice(0, own.lineEnds[last]);
}

/** Whether the JS/TS function a reference sits in binds `name` in a scope it can see. */
export function jsCodeBindsName(name: string, fn: Node, filePath: string, line: number, context: ResolutionContext): boolean {
  const text = jsFunctionCodeThrough(fn, filePath, line, context);
  const n = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  // A plain declaration. Destructuring re-binds what a call returns under
  // the same name — `const { t } = useI18n()` — which is the same-named
  // function more often than not.
  const declared = new RegExp(`\\b(?:const|let|var)\\s+${n}(?![\\w$])(?!\\s*[,\\]}])`).test(text);
  return declared || jsParameterListPattern(name, '').test(text);
}
