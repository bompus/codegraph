"""Probe the review's malformed-database suspicions on fresh copies of index F."""
import argparse
import json
import sqlite3
import subprocess
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('fix_round', type=Path)
args = parser.parse_args()
crate = Path(__file__).resolve().parents[1]
dest = args.fix_round / 'boundary-probes'
dest.mkdir(exist_ok=True)
query = dest / 'query.json'
query.write_text('[{"readSource":null,"includeExported":true,"limit":100000}]')
changes = {
    'ties': "UPDATE nodes SET file_path='docs/tie.ts',start_line=1,end_line=10,name='same'",
    'invalid-utf8': "UPDATE nodes SET docstring=CAST(x'666f6fff' AS TEXT)",
    'null-id': "UPDATE nodes SET id=NULL WHERE id=(SELECT id FROM nodes LIMIT 1)",
    'json-surrogate': r'''UPDATE nodes SET type_parameters='["\ud800","\u2028"]' ''',
    'json-separator': r'''UPDATE nodes SET type_parameters='["\u2028"]' ''',
    'json-overflow': "UPDATE nodes SET type_parameters='[1e999,-1e999,9007199254740993]'",
}
for name, sql in changes.items():
    started = time.perf_counter()
    root = dest / name
    (root / '.codegraph').mkdir(parents=True, exist_ok=True)
    original = args.fix_round / 'review-dead-code/idx/F/.codegraph/codegraph.db'
    source = sqlite3.connect(f'file:{original}?mode=ro', uri=True)
    copy = sqlite3.connect(root / '.codegraph/codegraph.db')
    source.backup(copy)
    source.close()
    copy.execute(sql)
    copy.commit()
    copy.close()
    commands = {
        'ts': ['fnm','exec','--using','codegraph','nice','-n','10','node',crate/'harness/run-ts.cjs',crate/'harness/dead-code.ts',root,query],
        'rs': ['nice','-n','10',crate/'target/debug/codegraph-rust-pilot','dead-code',root,query],
    }
    results = {}
    for language, command in commands.items():
        with (dest / f'{name}.{language}.json').open('wb') as out:
            result = subprocess.run(list(map(str, command)), cwd=crate.parent, stdout=out, stderr=subprocess.PIPE)
        results[language] = result.returncode
        (dest / f'{name}.{language}.err').write_bytes(result.stderr)
    comparison = subprocess.run(['cmp',str(dest/f'{name}.ts.json'),str(dest/f'{name}.rs.json')], capture_output=True)
    row = dict(case=name, seconds=time.perf_counter()-started, **results, cmp=comparison.returncode,
               sql=sql, commands={k:list(map(str,v)) for k,v in commands.items()})
    with (dest / 'results.jsonl').open('a') as out:
        out.write(json.dumps(row)+'\n')
    print(json.dumps(row), flush=True)
