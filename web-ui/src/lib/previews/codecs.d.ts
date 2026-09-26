// Types for the single-codec modules of hyparquet-compressors, imported one
// by one so the Parquet chunk carries only the decoders it uses (the package
// index also pulls a wasm snappy and the brotli dictionary).

declare module "hyparquet-compressors/src/gzip.js" {
  export function gunzip(input: Uint8Array, output?: Uint8Array): Uint8Array;
}

declare module "hyparquet-compressors/src/lz4.js" {
  export function decompressLz4(input: Uint8Array, outputLength: number): Uint8Array;
  export function decompressLz4Raw(input: Uint8Array, outputLength: number): Uint8Array;
}

declare module "hyparquet-compressors/src/brotli.js" {
  export function decompressBrotli(input: Uint8Array, outputLength: number): Uint8Array;
}
