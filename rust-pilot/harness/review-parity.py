"""Replay copied reviewer cases; retain commands, exits, byte diffs and timings."""
import argparse
import json
import re
import shlex
import subprocess
import time
from datetime import datetime
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('scratch', type=Path, help='fix-round directory containing copied review-* directories')
parser.add_argument('--performance', action='store_true')
parser.add_argument('--rounds', type=int, default=3)
parser.add_argument('--baseline', type=Path, help='pre-fix debug binary, for performance regression gates')
args = parser.parse_args()
crate = Path(__file__).resolve().parents[1]
receipts = args.scratch / ('performance-before.jsonl' if args.baseline else 'performance.jsonl' if args.performance else 'repros.jsonl')
failed = False


def run(command, output=None, cwd=crate.parent):
    start = datetime.now().astimezone().isoformat()
    clock = time.perf_counter()
    if output:
        with output.open('wb') as stream:
            result = subprocess.run(list(map(str, command)), cwd=cwd, stdout=stream, stderr=subprocess.PIPE)
        stdout = ''
    else:
        result = subprocess.run(list(map(str, command)), cwd=cwd, capture_output=True)
        stdout = result.stdout.decode(errors='replace')
    receipt = dict(start=start, end=datetime.now().astimezone().isoformat(), seconds=time.perf_counter()-clock,
                   cwd=str(cwd), command=shlex.join(map(str, command)), output=str(output) if output else None,
                   exit=result.returncode, stdout=stdout, stderr=result.stderr.decode(errors='replace'))
    with receipts.open('a') as f:
        f.write(json.dumps(receipt) + '\n')
    print(f"{receipt['command']}\nexit={result.returncode} seconds={receipt['seconds']:.6f} {stdout.strip()}", flush=True)
    return receipt


def compare(ts, rs):
    global failed
    result = run(['cmp', ts, rs])
    failed |= result['exit'] != 0


if args.performance:
    for module, name in [('nestjs', 'many1000'), ('nestjs', 'deep1500'), ('nestjs', 'deep3000'),
                         ('dead-code', 'P'), ('dead-code', 'exposed'), ('dead-code', 'json')]:
        for repeat in range(args.rounds):
            d = args.scratch / ('review-nestjs' if module == 'nestjs' else 'review-dead-code')
            inputs = [d / f'{name}.input.json'] if module == 'nestjs' else [d / 'idx' / name, d / 'qP.json']
            outputs = {}
            for implementation in (['debug'] if args.baseline else ['ts', 'debug', 'release']):
                out = args.scratch / f'perf-{name}-{repeat}-{implementation}{"-before" if args.baseline else ""}.json'
                if implementation == 'ts':
                    cmd = ['fnm', 'exec', '--using', 'codegraph', 'nice', '-n', '10', 'node',
                           crate / 'harness/run-ts.cjs', crate / ('harness/nest.ts' if module == 'nestjs' else 'harness/dead-code.ts'), *inputs]
                else:
                    cmd = ['nice', '-n', '10', args.baseline or crate / 'target' / implementation / 'codegraph-rust-pilot', module, *inputs]
                result = run(cmd, out)
                failed |= result['exit'] != 0
                outputs[implementation] = out
                # Wide margins over corrected runs; these reject the review's debug regressions.
                budget = {'many1000': 30, 'deep1500': 10, 'deep3000': 30, 'P': 10}.get(name)
                if implementation == 'debug' and budget is not None:
                    passed = result['seconds'] <= budget
                    print(f'Debug performance gate {name}: {result["seconds"]:.3f}s <= {budget}s: {passed}', flush=True)
                    failed |= not passed
            if args.baseline:
                continue
            for implementation in ['debug', 'release']:
                compare(outputs['ts'], outputs[implementation])
else:
    d = args.scratch / 'review-nestjs'
    names = ['m1', 'm1s', 'keyorder', 'integer-keys', 'manifest', 'dup', 'bigmodule', 'bigctl2',
             'sz100000', 'sz300000', 'sz500000', 'sz1000000', 'bc100000', 'bc200000', 'bc300000', 'bc400000',
             'many1000', 'deep1500', 'deep3000']
    for name in names:
        result = run(['bash', d / 'run.sh', name])
        failed |= result['exit'] != 0 or 'ts exit 0\nrs exit 0\nSAME' not in result['stdout']
        compare(d / f'{name}.ts.json', d / f'{name}.rs.json')
    d = args.scratch / 'review-dead-code'
    cases = [('A','qA.json'), *[(f'B{i}', 'qB.json') for i in range(8)],
             ('C1','qC.json'),('C3','qC.json'),('C3b','qC.json'),('D','qD.json'),
             *[(f'E{i}', 'qE.json') for i in range(5)],('F','qF.json'),('P','qP.json'),('A','qT.json'),
             *[(name, query) for name in ['exposed','json','gin','javalin','jq','petclinic','slim'] for query in ['q-mine0.json','q2.json']]]
    for name, query in cases:
        tag = 'final-' + Path(query).stem
        result = run(['bash', d / 'run.sh', name, d / query, tag])
        failed |= result['exit'] != 0 or re.search(r'ts_exit=0\([^)]*\) rs_exit=0\([^)]*\) cmp=0', result['stdout']) is None
        compare(d / f'out/{name}.{tag}.ts.json', d / f'out/{name}.{tag}.rs.json')
    root = d / 'idx/C3b'
    for implementation in ['ts', 'rs']:
        command = (['fnm','exec','--using','codegraph','nice','-n','10','node',crate/'harness/run-ts.cjs',crate/'harness/dead-code.ts']
                   if implementation == 'ts' else ['nice','-n','10',crate/'target/debug/codegraph-rust-pilot','dead-code'])
        result = run([*command,'.',d/'qB.json'], d/f'out/C3b.final-relative.{implementation}.json', root)
        failed |= result['exit'] != 0
    compare(d/'out/C3b.final-relative.ts.json',d/'out/C3b.final-relative.rs.json')
raise SystemExit(1 if failed else 0)
