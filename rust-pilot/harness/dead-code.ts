import fs from 'node:fs';
import CodeGraph from '../../src/codegraph';
import { buildDeadCodeReport, type DeadCodeQuery } from '../../src/graph/dead-code';
import { canonical } from './canonical';
const [root, queryFile] = process.argv.slice(2);
const cg = CodeGraph.openSync(root, {readOnly: true});
try {
  const queries = JSON.parse(fs.readFileSync(queryFile, 'utf8')) as DeadCodeQuery[];
  process.stdout.write(canonical(queries.map((query) => ({query, report: buildDeadCodeReport(cg, query)}))));
} finally {
  cg.close();
}
