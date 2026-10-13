"""Run both real implementations and retain exact commands, exit codes, and hashes."""
import argparse
import hashlib
import json
import shlex
import subprocess
from datetime import datetime
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('repo', type=Path)
parser.add_argument('scratch', type=Path)
parser.add_argument('groups', nargs='+', choices=['fixtures', 'edges', 'nest', 'flask', 'svelte', 'vite', 'vitest'])
args = parser.parse_args()
crate = args.repo / 'rust-pilot'
receipts = args.scratch / 'commands.jsonl'
cap = str(Path.home() / '.local/bin/run-capped')

def run(command, output=None):
    started = datetime.now().astimezone().isoformat()
    with (open(output, 'wb') if output else open(args.scratch / 'last-command.stdout', 'wb')) as stdout:
        result = subprocess.run(command, cwd=args.repo, stdout=stdout, stderr=subprocess.PIPE)
    text = shlex.join(map(str, command)) + (f' > {shlex.quote(str(output))}' if output else '')
    receipt = {'start': started, 'end': datetime.now().astimezone().isoformat(),
               'command': text, 'exit': result.returncode, 'stderr': result.stderr.decode(errors='replace')}
    if output and result.returncode == 0:
        receipt['sha256'] = hashlib.sha256(output.read_bytes()).hexdigest()
        receipt['bytes'] = output.stat().st_size
    with receipts.open('a') as file:
        file.write(json.dumps(receipt) + '\n')
    print(f'{text}\nexit {result.returncode}', flush=True)
    if result.stderr:
        print(result.stderr.decode(errors='replace'), flush=True)
    return result.returncode

failed = False
for group in args.groups:
    nest = group in ('fixtures', 'edges', 'nest')
    prefix = ('nest-' if nest else 'dead-') + group
    source = args.scratch / f'{prefix}.input.json' if nest else args.scratch / 'indexes' / group
    inputs = [str(source)] if nest else [str(source), str(crate / 'harness/dead-queries.json')]
    ts = args.scratch / f'{prefix}.ts.json'
    rs = args.scratch / f'{prefix}.rs.json'
    ts_exit = run(['fnm', 'exec', '--using', 'codegraph', cap, '6G', 'nice', '-n', '10', 'node',
                  str(crate / 'harness/run-ts.cjs'), str(crate / ('harness/nest.ts' if nest else 'harness/dead-code.ts')), *inputs], ts)
    rs_exit = run([cap, '6G', 'nice', '-n', '10', str(crate / 'target/debug/codegraph-rust-pilot'),
                  'nestjs' if nest else 'dead-code', *inputs], rs)
    diff_exit = run(['cmp', str(ts), str(rs)]) if ts_exit == 0 and rs_exit == 0 else None
    failed |= ts_exit != 0 or rs_exit != 0 or diff_exit != 0
    stamp = subprocess.check_output(['date', '+%F %T %Z'], text=True).strip()
    with (args.scratch / 'PILOT_LOG.md').open('a') as file:
        file.write(f'{stamp} parity {prefix}: TS exit {ts_exit}, Rust exit {rs_exit}, cmp exit {diff_exit}.\n')
raise SystemExit(1 if failed else 0)
