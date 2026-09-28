/**
 * Known-answer search checks on pinned real repositories. Run with
 * `npm run eval:search -- <corpus>` (search-baseline-runner.ts).
 *
 * Each expected symbol is named by name and file, because common names repeat:
 * `mount` has two definitions in svelte and `get` has dozens. A case passes
 * when every expected symbol is in the top `limit` results and the best of them
 * ranks at or above `maxRank`. A query with `path:`, `name:`, `kind:` or
 * `lang:` filters also fails if any returned row breaks a filter. A `scoped`
 * case must hold a target that the same text, unfiltered, ranks below the
 * limit; otherwise the case does not exercise scoping and fails as mislabeled.
 *
 * `knownMiss` cases record a measured limitation. They do not fail the run;
 * the report says FIXED when one starts to pass, so it can become a gating case.
 */
export type SearchCategory = 'exact' | 'prefix' | 'words' | 'typo' | 'filter' | 'scoped' | 'unicode';

export interface ExpectedHit {
  name: string;
  /** Substring of the node's file path. */
  file: string;
  kind?: string;
}

export interface SearchCase {
  id: string;
  corpus: string;
  category: SearchCategory;
  query: string;
  /** Empty means the query must return no rows. */
  expect: ExpectedHit[];
  /** Best expected rank must be at or above this. Default: limit. */
  maxRank?: number;
  limit?: number;
  knownMiss?: string;
}

export const searchCases: SearchCase[] = [
  // svelte
  { id: 'svelte-exact-dispatcher', corpus: 'svelte', category: 'exact', query: 'createEventDispatcher', maxRank: 1,
    expect: [{ name: 'createEventDispatcher', file: 'src/index-client.js' }, { name: 'createEventDispatcher', file: 'src/index-server.js' }] },
  { id: 'svelte-exact-mount', corpus: 'svelte', category: 'exact', query: 'mount', maxRank: 1,
    expect: [{ name: 'mount', file: 'internal/client/render.js', kind: 'function' }, { name: 'mount', file: 'src/index-server.js', kind: 'function' }] },
  { id: 'svelte-prefix-create-event', corpus: 'svelte', category: 'prefix', query: 'createEvent', maxRank: 1,
    expect: [{ name: 'createEventDispatcher', file: 'src/index-client.js' }, { name: 'createEventDispatcher', file: 'src/index-server.js' }] },
  { id: 'svelte-prefix-search-param', corpus: 'svelte', category: 'prefix', query: 'SvelteURLSearchParam', maxRank: 1,
    expect: [{ name: 'SvelteURLSearchParams', file: 'reactivity/url-search-params.js', kind: 'class' }] },
  { id: 'svelte-typo-dispatcher', corpus: 'svelte', category: 'typo', query: 'createEventDispatchr', maxRank: 1,
    expect: [{ name: 'createEventDispatcher', file: 'src/index-client.js' }] },
  { id: 'svelte-typo-transposed', corpus: 'svelte', category: 'typo', query: 'SvelteMpa', maxRank: 1,
    expect: [{ name: 'SvelteMap', file: 'reactivity/map.js', kind: 'class' }] },
  { id: 'svelte-filter-kind-name-path', corpus: 'svelte', category: 'filter', query: 'kind:function name:mount path:packages/svelte/src/internal',
    expect: [{ name: 'mount', file: 'internal/client/render.js' }, { name: '_mount', file: 'internal/client/render.js' }, { name: 'unmount', file: 'internal/client/render.js' }] },
  { id: 'svelte-filter-path-kind-only', corpus: 'svelte', category: 'filter', query: 'path:reactivity kind:class',
    expect: [{ name: 'SvelteMap', file: 'reactivity/map.js' }, { name: 'SvelteSet', file: 'reactivity/set.js' }, { name: 'SvelteDate', file: 'reactivity/date.js' }] },
  { id: 'svelte-filter-lang-kind-text', corpus: 'svelte', category: 'filter', query: 'lang:typescript kind:interface Component', maxRank: 4,
    expect: [{ name: 'Component', file: 'packages/svelte/src/index.d.ts' }] },
  // The only `dispatch` under tests is a constant, so kind:function leaves nothing.
  { id: 'svelte-filter-empty', corpus: 'svelte', category: 'filter', query: 'kind:function lang:typescript path:packages/svelte/tests name:dispatch',
    expect: [] },
  { id: 'svelte-scoped-set', corpus: 'svelte', category: 'scoped', query: 'path:url-search-params set', maxRank: 1,
    expect: [{ name: 'set', file: 'reactivity/url-search-params.js', kind: 'method' }] },
  // The same identifier spelled as spaced words (compare svelte-exact-dispatcher).
  { id: 'svelte-words-camel', corpus: 'svelte', category: 'words', query: 'event dispatcher', maxRank: 5,
    expect: [{ name: 'createEventDispatcher', file: 'src/index-client.js' }] },
  { id: 'svelte-limit-camel-words', corpus: 'svelte', category: 'words', query: 'svelte map',
    expect: [{ name: 'SvelteMap', file: 'reactivity/map.js', kind: 'class' }] },
  { id: 'svelte-limit-inflection', corpus: 'svelte', category: 'prefix', query: 'mounting',
    expect: [{ name: 'mount', file: 'internal/client/render.js' }] },

  // flask
  { id: 'flask-exact-url-for', corpus: 'flask', category: 'exact', query: 'url_for', maxRank: 1,
    expect: [{ name: 'url_for', file: 'src/flask/helpers.py', kind: 'function' }] },
  { id: 'flask-exact-class', corpus: 'flask', category: 'exact', query: 'Flask', maxRank: 1,
    expect: [{ name: 'Flask', file: 'src/flask/app.py', kind: 'class' }] },
  { id: 'flask-prefix-send-from', corpus: 'flask', category: 'prefix', query: 'send_from', maxRank: 1,
    expect: [{ name: 'send_from_directory', file: 'src/flask/helpers.py' }] },
  { id: 'flask-words-snake', corpus: 'flask', category: 'words', query: 'url for', maxRank: 1,
    expect: [{ name: 'url_for', file: 'src/flask/helpers.py' }] },
  { id: 'flask-typo-blueprint', corpus: 'flask', category: 'typo', query: 'Bluprint', maxRank: 3,
    expect: [{ name: 'Blueprint', file: 'src/flask/blueprints.py', kind: 'class' }] },
  { id: 'flask-filter-path-kind', corpus: 'flask', category: 'filter', query: 'path:src/flask/sansio kind:class',
    expect: [{ name: 'App', file: 'sansio/app.py' }, { name: 'Blueprint', file: 'sansio/blueprints.py' }, { name: 'Scaffold', file: 'sansio/scaffold.py' }] },
  { id: 'flask-filter-method-name', corpus: 'flask', category: 'filter', query: 'kind:method path:src/flask/app.py name:handle',
    expect: [{ name: 'handle_exception', file: 'src/flask/app.py' }, { name: 'handle_http_exception', file: 'src/flask/app.py' }, { name: 'handle_user_exception', file: 'src/flask/app.py' }] },
  { id: 'flask-scoped-session-transaction', corpus: 'flask', category: 'scoped', query: 'path:src/flask/testing.py open', maxRank: 2,
    expect: [{ name: 'session_transaction', file: 'src/flask/testing.py' }] },
  { id: 'flask-unicode-lower', corpus: 'flask', category: 'unicode', query: 'киртест', maxRank: 1,
    expect: [{ name: 'GET /киртест', file: 'tests/test_basic.py', kind: 'route' }] },
  { id: 'flask-unicode-upper', corpus: 'flask', category: 'unicode', query: 'КИРТЕСТ', maxRank: 1,
    expect: [{ name: 'GET /киртест', file: 'tests/test_basic.py', kind: 'route' }] },
  { id: 'flask-unicode-name-filter', corpus: 'flask', category: 'unicode', query: 'name:КИРТЕСТ', maxRank: 1,
    expect: [{ name: 'GET /киртест', file: 'tests/test_basic.py', kind: 'route' }] },
  { id: 'flask-limit-spaced-words', corpus: 'flask', category: 'words', query: 'add url rule', maxRank: 1,
    expect: [{ name: 'add_url_rule', file: 'sansio/app.py' }] },
];
