// Wasm smoke test: loads the bundle built by scripts/build-wasm.sh in Node,
// feeds it every fixture demo and checks that it returns the same HTML as the
// native CLI (`demoscope --html`).
//
//   bash scripts/build-wasm.sh
//   node scripts/check-wasm.mjs [fixtures-dir]
//
// Needs the release binary (cargo build --release) and Node 18+.
import { readFileSync, readdirSync, mkdtempSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const fixtures = resolve(process.argv[2] ?? join(root, 'tests', 'fixtures'));
const bin = join(root, 'target', 'release', process.platform === 'win32' ? 'demoscope.exe' : 'demoscope');

const glue = await import(pathToFileURL(join(root, 'web', 'demoscope.js')).href);
await glue.default({ module_or_path: readFileSync(join(root, 'web', 'demoscope_bg.wasm')) });

const cargoToml = readFileSync(join(root, 'Cargo.toml'), 'utf8');
const crateVersion = /^version = "([^"]+)"/m.exec(cargoToml)[1];
if (glue.version() !== crateVersion) {
  console.error(`FAIL  wasm version ${glue.version()} != Cargo.toml ${crateVersion}`);
  process.exit(1);
}

const demos = readdirSync(fixtures)
  .filter((n) => /\.(dem|dm2|mvd|qwd|dm_\d+)$/i.test(n))
  .sort();
if (demos.length === 0) {
  console.error(`FAIL  no demos in ${fixtures}`);
  process.exit(1);
}

const tmp = mkdtempSync(join(tmpdir(), 'demoscope-wasm-'));
let failed = 0;
try {
  for (const name of demos) {
    const path = join(fixtures, name);
    let wasmHtml;
    try {
      wasmHtml = glue.parse_demo_to_html(new Uint8Array(readFileSync(path)), undefined, name, 0, undefined, '');
    } catch (e) {
      console.error(`FAIL  ${name}: wasm threw: ${e}`);
      failed++;
      continue;
    }
    const out = join(tmp, `${name}.html`);
    execFileSync(bin, [path, '--html', out], { stdio: 'ignore' });
    const nativeHtml = readFileSync(out, 'utf8');
    if (!wasmHtml.includes('const META') || wasmHtml.length < 10000) {
      console.error(`FAIL  ${name}: wasm output is not a viewer (${wasmHtml.length} chars)`);
      failed++;
    } else if (wasmHtml !== nativeHtml) {
      console.error(`FAIL  ${name}: wasm and native HTML differ (${wasmHtml.length} vs ${nativeHtml.length} chars)`);
      failed++;
    } else {
      console.log(`PASS  ${name} (${wasmHtml.length} chars, identical to native)`);
    }
  }
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
console.log(`${demos.length - failed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
