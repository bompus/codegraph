// Use the installed compiler; no package install or generated production files.
const fs = require('node:fs');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => {
  const output = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
    fileName: filename,
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true},
  });
  module._compile(output.outputText, filename);
};
process.argv.splice(1, 1);
require(process.argv[1]);
