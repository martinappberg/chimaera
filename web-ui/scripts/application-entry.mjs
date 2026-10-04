import { constants } from "node:fs";
import { open, realpath, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
const virtual = "virtual:chimaera-application-entry";
const internal = `\0${virtual}`;
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const jsName = (value) => typeof value === "string" && /^[A-Za-z0-9_.-]+\.js$/.test(value);
const digest = (value) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const relative = (value) => typeof value === "string" && value.length <= 4096 && !value.startsWith("/") &&
  value.split("/").every((piece) => piece.length > 0 && piece !== "." && piece !== "..");
async function bounded(path, max = 16 * 1024 * 1024) {
  const fd = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const before = await fd.stat();
    if (!before.isFile() || before.size > max) throw new Error("Invalid captured presentation file");
    const bytes = Buffer.alloc(before.size + 1); let length = 0;
    while (length < bytes.length) {
      const read = await fd.read(bytes, length, bytes.length - length, null);
      if (read.bytesRead === 0) break; length += read.bytesRead;
    }
    const after = await fd.stat();
    if (length !== before.size || after.size !== before.size || after.mtimeMs !== before.mtimeMs || after.ctimeMs !== before.ctimeMs) {
      throw new Error("Captured presentation changed");
    }
    return bytes.subarray(0, length);
  } finally { await fd.close(); }
}
/** Build-time only: default omission never inspects a private package. These
 * provenance checks are not signing, entitlement or installation authority. */
export async function captureApplicationEntry(selected, publicRoot) {
  if (selected === undefined || selected === "") return null;
  if (!isAbsolute(selected) || await realpath(selected) !== resolve(selected) || !selected.endsWith("/dist/pro-client-ui.js")) {
    throw new Error("Expected captured application entry");
  }
  const directory = dirname(selected), packageRoot = dirname(directory);
  const files = new Map(); let total = 0;
  const take = async (path, limit) => {
    if (await realpath(path) !== resolve(path)) throw new Error("Linked application input refused");
    const bytes = await bounded(path, limit); total += bytes.length;
    if (total > 64 * 1024 * 1024) throw new Error("Application input ceiling exceeded");
    files.set(path, sha(bytes)); return bytes;
  };
  const css = await take(join(directory, "pro-client-ui.css"), 2 * 1024 * 1024);
  if (/@import|url\s*\(/i.test(css.toString("utf8"))) throw new Error("Application external style refused");
  const receipt = JSON.parse(await take(join(directory, "verified-closure.json"), 2 * 1024 * 1024));
  const inputs = JSON.parse(await take(join(directory, "build-input.json"), 256 * 1024));
  const compiler = JSON.parse(await take(join(directory, "build-closure.json"), 2 * 1024 * 1024));
  if (inputs.version !== 1 || inputs.installed !== false || inputs.runtimeEnabled !== false || receipt.version !== 1 || receipt.hostModuleReceiptChecked !== true || receipt.installed !== false || receipt.runtimeEnabled !== false ||
    compiler.version !== 1 || compiler.hostImplementationModules !== 0 || !Array.isArray(receipt.chunks) ||
    receipt.chunks.length < 1 || receipt.chunks.length > 512 || !Array.isArray(compiler.chunks) || compiler.chunks.length !== receipt.chunks.length) {
    throw new Error("Unverified application closure");
  }
  const publicFiles = { publicPackageSha256: "package.json", publicLockSha256: "package-lock.json",
    sveltePackageSha256: "node_modules/svelte/package.json", publicInterfaceSha256: "src/lib/extensions/application.ts",
    publicKeptInterfaceSha256: "src/lib/extensions/keptReview.ts" };
  for (const [field, file] of Object.entries(publicFiles)) {
    const bytes = await take(join(publicRoot, file), 2 * 1024 * 1024);
    if (inputs[field] !== sha(bytes) || receipt.capturedInputs?.[field] !== inputs[field]) throw new Error("Application SDK input drift");
  }
  if (!Array.isArray(inputs.privateSources) || inputs.privateSources.length < 1 || inputs.privateSources.length > 128) throw new Error("Invalid application sources");
  const sources = new Set();
  for (const item of inputs.privateSources) {
    if (!relative(item.file) || !digest(item.sha256) || sources.has(item.file)) throw new Error("Invalid application source");
    sources.add(item.file);
    if (sha(await take(join(packageRoot, item.file), 2 * 1024 * 1024)) !== item.sha256) throw new Error("Application source drift");
  }
  const lexerPackage = JSON.parse(await take(join(publicRoot, "node_modules/es-module-lexer/package.json"), 64 * 1024));
  const lock = JSON.parse(await bounded(join(publicRoot, "package-lock.json"), 2 * 1024 * 1024));
  if (lexerPackage.version !== lock.packages?.["node_modules/es-module-lexer"]?.version) throw new Error("Application parser drift");
  const lexer = await import(pathToFileURL(join(publicRoot, "node_modules/es-module-lexer/dist/lexer.js")).href); await lexer.init;
  const graph = new Map();
  for (const chunk of receipt.chunks) {
    if (!jsName(chunk.file) || !digest(chunk.sha256) || graph.has(chunk.file)) throw new Error("Invalid application chunk");
    const bytes = await take(join(directory, chunk.file));
    if (bytes.length === 0 || sha(bytes) !== chunk.sha256) throw new Error("Application bytes drift");
    const declared = [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])];
    const actual = [];
    for (const item of lexer.parse(bytes.toString("utf8"))[0]) {
      if (item.d === -2) continue;
      if (typeof item.n !== "string" || !item.n.startsWith("./") || !jsName(item.n.slice(2))) throw new Error("Application external import refused");
      actual.push(item.n.slice(2));
    }
    if (JSON.stringify([...new Set(actual)].sort()) !== JSON.stringify([...new Set(declared)].sort())) throw new Error("Application graph drift");
    const provenance = compiler.chunks.find((item) => item.file === chunk.file);
    if (!provenance || provenance.sha256 !== chunk.sha256 || !Array.isArray(provenance.modules) || provenance.modules.length > 4096 ||
      provenance.modules.some((id) => typeof id !== "string" || id.split("?")[0].startsWith(join(publicRoot, "src") + "/"))) throw new Error("Application host module copy refused");
    graph.set(chunk.file, actual);
  }
  const names = (await readdir(directory)).filter((name) => name.endsWith(".js")).sort();
  if (JSON.stringify(names) !== JSON.stringify([...graph.keys()].sort())) throw new Error("Unrecorded application chunk");
  const reached = new Set(); const visit = (name) => {
    if (!graph.has(name)) throw new Error("Missing application chunk");
    if (reached.has(name)) return; reached.add(name); for (const next of graph.get(name)) visit(next);
  }; visit("pro-client-ui.js");
  if (reached.size !== graph.size) throw new Error("Unreachable application chunk");
  return { entry: selected, directory, files,
    async verify() { for (const [file, hash] of files) if (sha(await bounded(file)) !== hash) throw new Error("Application input changed during assembly"); },
    receipt: { version: 1, selected: true, installedAuthority: false,
      privateVerifiedReceipt: files.get(join(directory, "verified-closure.json")),
      privateInputsReceipt: files.get(join(directory, "build-input.json")),
      inputFiles: [...files].map(([file, sha256]) => ({ file, sha256 })) },
  };
}
export function applicationEntryPlugin(publicRoot, selected = process.env.CHIMAERA_APPLICATION_ENTRY) {
  let capture = null;
  return { name: "chimaera-selected-application-entry",
    async buildStart() { capture = await captureApplicationEntry(selected, publicRoot); },
    resolveId(id) { return id === virtual ? internal : null; },
    load(id) {
      if (id !== internal) return null;
      return capture === null ? "export const loadApplicationEntry = null;" :
        `import ${JSON.stringify(join(capture.directory, "pro-client-ui.css"))}; export const loadApplicationEntry = () => import(${JSON.stringify(capture.entry)});`;
    },
    generateBundle: { order: "post", async handler(_options, bundle) {
      if (capture !== null) await capture.verify();
      const emitted = Object.values(bundle).filter((item) => item.type === "chunk");
      this.emitFile({ type: "asset", fileName: "application-assembly.json", source: JSON.stringify({
        ...(capture?.receipt ?? { version: 1, selected: false, installedAuthority: false }),
        chunks: emitted.map((chunk) => ({ file: chunk.fileName, sha256: sha(Buffer.from(chunk.code)), imports: chunk.imports, dynamicImports: chunk.dynamicImports })),
      }, null, 2) + "\n" });
    } },
  };
}
