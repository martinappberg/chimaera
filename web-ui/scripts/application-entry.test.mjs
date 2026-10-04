import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, rm, realpath, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { captureApplicationEntry, applicationEntryPlugin } from "./application-entry.mjs";
const publicRoot = await realpath(dirname(dirname(fileURLToPath(import.meta.url))));
const sha = (value) => createHash("sha256").update(value).digest("hex");
async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), "chimaera-application-entry-test-")));
  const dist = join(root, "dist"); await mkdir(dist); await mkdir(join(root, "src"));
  await writeFile(join(root, "src/index.ts"), "export default {};\n");
  await writeFile(join(dist, "pro-client-ui.css"), ".kept { color: var(--fg); }\n");
  const capturedInputs = {};
  for (const [field, file] of Object.entries({ publicPackageSha256: "package.json", publicLockSha256: "package-lock.json",
    sveltePackageSha256: "node_modules/svelte/package.json", publicInterfaceSha256: "src/lib/extensions/application.ts",
    publicKeptInterfaceSha256: "src/lib/extensions/keptReview.ts" })) capturedInputs[field] = sha(await readFile(join(publicRoot, file)));
  const input = { version: 1, installed: false, runtimeEnabled: false, ...capturedInputs,
    privateSources: [{ file: "src/index.ts", sha256: sha(await readFile(join(root, "src/index.ts"))) }] };
  const writeGraph = async (code = 'import "./view.js"; export default {};', imports = ["view.js"]) => {
    await writeFile(join(dist, "pro-client-ui.js"), code); await writeFile(join(dist, "view.js"), "export const view = 1;");
    const chunks = [{ file: "pro-client-ui.js", sha256: sha(code), imports, dynamicImports: [] },
      { file: "view.js", sha256: sha("export const view = 1;"), imports: [], dynamicImports: [] }];
    await writeFile(join(dist, "build-input.json"), JSON.stringify(input));
    await writeFile(join(dist, "verified-closure.json"), JSON.stringify({ version: 1, chunks, capturedInputs, hostModuleReceiptChecked: true, installed: false, runtimeEnabled: false }));
    await writeFile(join(dist, "build-closure.json"), JSON.stringify({ version: 1, hostImplementationModules: 0,
      chunks: chunks.map((chunk) => ({ ...chunk, modules: [join(root, "src/index.ts")] })) }));
  };
  await writeGraph(); return { root, dist, entry: join(dist, "pro-client-ui.js"), writeGraph };
}
test("absent entry never inspects private inputs and produces a closed null facade", async () => {
  assert.equal(await captureApplicationEntry(undefined, "/nonexistent"), null);
  const plugin = applicationEntryPlugin("/nonexistent", ""); await plugin.buildStart();
  assert.equal(plugin.load(plugin.resolveId("virtual:chimaera-application-entry")), "export const loadApplicationEntry = null;");
});
test("actual selected capture checks graph/source/bytes and detects mid-assembly drift", async () => {
  const f = await fixture(); try {
    // join() emits backslashes on Windows. This actual capture must reach all
    // byte/graph checks with the same closed dist/entry shape on every runner.
    assert.equal(basename(dirname(f.entry)), "dist"); assert.equal(basename(f.entry), "pro-client-ui.js");
    const capture = await captureApplicationEntry(f.entry, publicRoot); assert.ok(capture); await capture.verify();
    await writeFile(join(f.root, "src/index.ts"), "changed"); await assert.rejects(capture.verify(), /changed/);
  } finally { await rm(f.root, { recursive: true }); }
});
test("self-consistent receipts cannot hide an empty entry or external/computed import", async () => {
  const f = await fixture(); try {
    await f.writeGraph("", []); await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /bytes/);
    await f.writeGraph('import "https://fixture.invalid/a.js";', []); await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /external/);
    await f.writeGraph('import(globalThis.name);', []); await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /external/);
  } finally { await rm(f.root, { recursive: true }); }
});
test("linked entry, remote CSS and unrecorded chunks refuse", async () => {
  const f = await fixture(); try {
    await writeFile(join(f.dist, "pro-client-ui.css"), '@import "https://fixture.invalid/style";');
    await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /style/);
    await writeFile(join(f.dist, "pro-client-ui.css"), ""); await writeFile(join(f.dist, "extra.js"), "export default 0;");
    await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /Unrecorded/);
    await symlink(f.entry, join(f.dist, "linked.js")); await assert.rejects(captureApplicationEntry(join(f.dist, "linked.js"), publicRoot));
  } finally { await rm(f.root, { recursive: true }); }
});

test("host implementation provenance refuses either emitted path separator", async () => {
  const f = await fixture(); try {
    for (const separator of ["/", "\\"]) {
      const compiler = JSON.parse(await readFile(join(f.dist, "build-closure.json"), "utf8"));
      compiler.chunks[0].modules = [join(publicRoot, "src", "lib", "net", "api.ts").replaceAll("\\", "/").replaceAll("/", separator) + "?fixture"];
      await writeFile(join(f.dist, "build-closure.json"), JSON.stringify(compiler));
      await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /host module copy refused/);
    }
  } finally { await rm(f.root, { recursive: true }); }
});
test("receipt source paths retain their closed forward-slash grammar on every platform", async () => {
  const f = await fixture(); try {
    const input = JSON.parse(await readFile(join(f.dist, "build-input.json"), "utf8"));
    // A backslash can become a traversal separator on Windows. Never interpret
    // a different source namespace from the verifier's forward-slash receipt.
    for (const file of ["src\\index.ts", "..\\index.ts"]) {
      input.privateSources[0].file = file;
      await writeFile(join(f.dist, "build-input.json"), JSON.stringify(input));
      await assert.rejects(captureApplicationEntry(f.entry, publicRoot), /Invalid application source/);
    }
  } finally { await rm(f.root, { recursive: true }); }
});
