/**
 * Parquet over ranged `/raw` reads (hyparquet). Only the bytes a grid page
 * needs cross the tunnel and the daemon holds nothing: the first request is a
 * suffix read of the footer (which also tells the file's size), then each page
 * of rows reads just the column-chunk bytes under it.
 *
 * A file written with an offset index (Spark, parquet-mr, pyarrow with
 * `write_page_index`) says where each data page starts, and hyparquet reads
 * only the pages a range needs. Most files don't carry one, and a single large
 * row group (pyarrow's default for up to ~1M rows) would then cost the whole
 * file per read; for those, this walks the page headers of the flat column
 * chunks lazily (a few hundred bytes each, only as far as the rows asked for)
 * and hands hyparquet the same page locations an offset index would.
 */

import {
  parquetMetadataAsync,
  parquetRead,
  parquetSchema,
  type AsyncBuffer,
  type ColumnMetaData,
  type CompressionCodec,
  type Compressors,
  type FileMetaData,
  type ParquetReadOptions,
  type SchemaTree,
} from "hyparquet";
import { deserializeTCompactProtocol } from "hyparquet/src/thrift.js";
import { gunzip } from "hyparquet-compressors/src/gzip.js";
import { decompressLz4, decompressLz4Raw } from "hyparquet-compressors/src/lz4.js";
import { decompress as zstd } from "fzstd";
import { fsRawUrl } from "./files";
import {
  ByteCache,
  chunkSpan,
  contentRangeTotal,
  groupSpans,
  groupsForRange,
  walkCovers,
  type CellKind,
  type GroupSpan,
} from "./parquetGrid";

/** Footer bytes fetched up front (most footers fit; a larger one costs one more read). */
const TAIL_BYTES = 64 * 1024;
/** Cap on cached column bytes (a bounded window over any size of file). */
const CACHE_BYTES = 48 * 1024 * 1024;
/** Chunks at most this big are read whole (one request beats a header walk). */
const WALK_MIN_BYTES = 1024 * 1024;
/** Page-header read windows, widened when a header carries big statistics. */
const HEADER_WINDOWS = [512, 16 * 1024, 256 * 1024, 2 * 1024 * 1024];
/**
 * How a walk reads headers. Arrow caps a page at 20,000 rows, so a 1M-row
 * chunk has ~50 pages, and a deep jump reads every header before it: one
 * round trip each (a few hundred bytes), or wide windows that carry several
 * headers (and the pages between them) per round trip. Which is cheaper is
 * the link's bandwidth-delay product: when a page costs less to transfer
 * than a round trip does, walking through windows wins. Measured on a
 * simulated 50 ms / 10 MB/s tunnel, a jump to row 700,000 of a 16 MB file:
 * ~0.9 s and 8.5 MB windowed, ~2.1 s and 1.4 MB header by header.
 */
const WALK_WINDOW = 512 * 1024;
/** Pages under this always walk windowed (their headers are too dense). */
const TINY_PAGE = 16 * 1024;

/** Codecs hyparquet decodes itself. */
const BUILTIN: CompressionCodec[] = ["UNCOMPRESSED", "SNAPPY"];

export class ParquetError extends Error {}

/** A data page's place in its chunk, as an offset index states it (hyparquet's
 *  own shape; its package index doesn't export the type). */
interface PageLocation {
  offset: bigint;
  compressed_page_size: number;
  first_row_index: bigint;
}

/** One top-level column: its name, how its cells read, and its schema text. */
export interface ParquetColumn {
  name: string;
  kind: CellKind;
}

interface ChunkWalk {
  pages: PageLocation[];
  next: number;
  /** Group-relative first row of the next page. */
  rows: number;
  end: number;
  done: boolean;
  /** Serializes walks of one chunk (two grid pages may ask at once). */
  lock: Promise<void>;
}

/** The ranged view of one `/raw` file, re-minting its ticket when it ages out. */
class RangedFile implements AsyncBuffer {
  fetched = 0;
  requests = 0;
  /** Fastest small read seen (ms): the link's round trip, roughly. */
  private rtt = Infinity;
  /** Transfer rate of the latest large read (bytes/ms), once there is one. */
  private rate = 0;
  private inflight = new Map<string, Promise<ArrayBuffer>>();
  constructor(
    private readonly path: string,
    private url: string,
    readonly byteLength: number,
    private readonly cache: ByteCache,
  ) {}

  /** The link's bandwidth-delay product in bytes (Infinity until measured:
   *  a tunnel is the case worth assuming). */
  get bdp(): number {
    return this.rate > 0 && Number.isFinite(this.rtt) ? this.rate * this.rtt : Infinity;
  }

  /** `[start, end)` only if cached (never a request). */
  peek(start: number, end: number): ArrayBuffer | null {
    return this.cache.get(Math.max(0, start), Math.min(this.byteLength, end));
  }

  slice(start: number, end: number = this.byteLength): Promise<ArrayBuffer> {
    const s = Math.max(0, start);
    const e = Math.min(this.byteLength, end);
    if (e <= s) return Promise.resolve(new ArrayBuffer(0));
    const hit = this.cache.get(s, e);
    if (hit !== null) return Promise.resolve(hit);
    const key = `${s}-${e}`;
    const pending = this.inflight.get(key);
    if (pending !== undefined) return pending;
    const load = this.fetchRange(s, e).finally(() => this.inflight.delete(key));
    this.inflight.set(key, load);
    return load;
  }

  private async fetchRange(start: number, end: number): Promise<ArrayBuffer> {
    for (let attempt = 0; ; attempt++) {
      this.requests += 1;
      const t0 = performance.now();
      const res = await fetch(this.url, { headers: { Range: `bytes=${start}-${end - 1}` } });
      if (res.status === 404 && attempt === 0) {
        // Tickets live ~10 minutes; an open grid outlives them.
        await res.body?.cancel().catch(() => {});
        this.url = await fsRawUrl(this.path);
        continue;
      }
      if (res.status !== 206 && res.status !== 200) {
        await res.body?.cancel().catch(() => {});
        throw new ParquetError(res.status === 404 ? "the file is gone" : `a read failed (${res.status})`);
      }
      const whole = await res.arrayBuffer();
      this.fetched += whole.byteLength;
      this.measure(whole.byteLength, performance.now() - t0);
      // A server that ignored the range sent everything: keep only the span.
      const buf = res.status === 200 ? whole.slice(start, end) : whole;
      this.cache.put(start, buf);
      return buf;
    }
  }

  /** Concurrent reads share the link, so both figures are rough: good enough
   *  to pick a walk strategy, never used for anything that must be right. */
  private measure(bytes: number, ms: number): void {
    if (bytes <= 16 * 1024) this.rtt = Math.min(this.rtt, Math.max(1, ms));
    else if (bytes >= 256 * 1024 && Number.isFinite(this.rtt)) {
      this.rate = bytes / Math.max(1, ms - this.rtt);
    }
  }
}

export interface ParquetFacts {
  rows: number;
  rowGroups: number;
  size: number;
  createdBy: string | null;
  codecs: CompressionCodec[];
  /** Whether any column chunk carries an offset index. */
  pageIndex: boolean;
  keyValueKeys: string[];
}

export class ParquetSource {
  readonly columns: ParquetColumn[];
  readonly schema: SchemaTree;
  readonly facts: ParquetFacts;
  private readonly spans: GroupSpan[];
  private readonly walks = new Map<string, ChunkWalk>();
  private readonly compressors: Compressors;

  private constructor(
    private readonly file: RangedFile,
    readonly metadata: FileMetaData,
    compressors: Compressors,
  ) {
    this.compressors = compressors;
    this.schema = parquetSchema(metadata);
    this.columns = this.schema.children.map((c) => ({ name: c.element.name, kind: cellKind(c) }));
    this.spans = groupSpans(metadata.row_groups.map((g) => Number(g.num_rows)));
    const codecs = new Set<CompressionCodec>();
    let pageIndex = false;
    for (const g of metadata.row_groups) {
      for (const c of g.columns) {
        if (c.meta_data !== undefined) codecs.add(c.meta_data.codec);
        if (c.offset_index_offset !== undefined && c.offset_index_length) pageIndex = true;
      }
    }
    this.facts = {
      rows: Number(metadata.num_rows),
      rowGroups: metadata.row_groups.length,
      size: file.byteLength,
      createdBy: metadata.created_by ?? null,
      codecs: [...codecs],
      pageIndex,
      keyValueKeys: (metadata.key_value_metadata ?? []).map((kv) => kv.key),
    };
  }

  /** Bytes read over the network so far (footer included), and in how many requests. */
  get traffic(): { bytes: number; requests: number } {
    return { bytes: this.file.fetched, requests: this.file.requests };
  }

  /** Open `path`: one suffix read for the footer, a second only for a big one. */
  static async open(path: string): Promise<ParquetSource> {
    let url = await fsRawUrl(path);
    let res = await fetch(url, { headers: { Range: `bytes=-${TAIL_BYTES}` } });
    if (res.status === 404) {
      await res.body?.cancel().catch(() => {});
      url = await fsRawUrl(path);
      res = await fetch(url, { headers: { Range: `bytes=-${TAIL_BYTES}` } });
    }
    if (res.status === 416) throw new ParquetError("the file is empty");
    if (res.status !== 206 && res.status !== 200) {
      await res.body?.cancel().catch(() => {});
      throw new ParquetError(res.status === 404 ? "the file is gone" : `the file could not be read (${res.status})`);
    }
    const tail = await res.arrayBuffer();
    const size = res.status === 200 ? tail.byteLength : contentRangeTotal(res.headers.get("content-range"));
    if (size === null) throw new ParquetError("the daemon did not say how big the file is");
    if (size < 12) throw new ParquetError("not a Parquet file (too short)");
    const cache = new ByteCache(CACHE_BYTES);
    const file = new RangedFile(path, url, size, cache);
    file.fetched = tail.byteLength;
    file.requests = 1;
    cache.put(size - tail.byteLength, tail);
    const magic = new Uint8Array(tail, tail.byteLength - 4, 4);
    if (String.fromCharCode(...magic) !== "PAR1") {
      throw new ParquetError(
        String.fromCharCode(...magic) === "PARE" ? "this Parquet file is encrypted" : "not a Parquet file",
      );
    }
    let metadata: FileMetaData;
    try {
      metadata = await parquetMetadataAsync(file, { initialFetchSize: tail.byteLength });
    } catch (e) {
      throw new ParquetError(`the footer could not be read: ${e instanceof Error ? e.message : String(e)}`);
    }
    const codecs = new Set<CompressionCodec>();
    for (const g of metadata.row_groups) {
      for (const c of g.columns) if (c.meta_data !== undefined) codecs.add(c.meta_data.codec);
    }
    const compressors: Compressors = {
      GZIP: (input, length) => gunzip(input, new Uint8Array(length)),
      ZSTD: (input) => zstd(input),
      LZ4: decompressLz4,
      LZ4_RAW: decompressLz4Raw,
    };
    if (codecs.has("BROTLI")) {
      // Brotli's dictionary alone is ~70 KB gzipped: only files that use it pay.
      const { decompressBrotli } = await import("hyparquet-compressors/src/brotli.js");
      compressors.BROTLI = decompressBrotli;
    }
    return new ParquetSource(file, metadata, compressors);
  }

  /** Codecs in the file no decoder here handles (LZO, today). */
  unsupportedCodecs(): CompressionCodec[] {
    return this.facts.codecs.filter((c) => !BUILTIN.includes(c) && this.compressors[c] === undefined);
  }

  /** Rows `[start, end)` as arrays in column order. */
  async read(start: number, end: number): Promise<unknown[][]> {
    if (end <= start) return [];
    const bad = this.unsupportedCodecs();
    if (bad.length > 0) throw new ParquetError(`${bad.join(", ")} compression isn't supported here`);
    const pageLocationsByGroup = this.metadata.row_groups.map(() => ({}) as Record<string, PageLocation[]>);
    const walks: Promise<void>[] = [];
    for (const { span, from, to } of groupsForRange(this.spans, start, end - start)) {
      if (from === 0 && to === span.rows) continue; // the whole group: plain chunk reads
      const group = this.metadata.row_groups[span.index];
      for (const chunk of group.columns) {
        const meta = chunk.meta_data;
        if (meta === undefined || chunk.offset_index_length) continue;
        if (Number(meta.total_compressed_size) <= WALK_MIN_BYTES || !this.isFlat(meta)) continue;
        const key = `${span.index}:${meta.path_in_schema.join(".")}`;
        walks.push(
          this.walkTo(key, meta, span.rows, to).then((pages) => {
            if (pages !== null) pageLocationsByGroup[span.index][meta.path_in_schema.join(".")] = pages;
          }),
        );
      }
    }
    await Promise.all(walks);
    const options: ParquetReadOptions & { pageLocationsByGroup: Record<string, PageLocation[]>[] } = {
      file: this.file,
      metadata: this.metadata,
      columns: this.columns.map((c) => c.name),
      rowStart: start,
      rowEnd: end,
      compressors: this.compressors,
      useOffsetIndex: true,
      rowFormat: "array",
      pageLocationsByGroup,
    };
    let rows: unknown[][] = [];
    options.onComplete = (r: unknown[][]) => (rows = r);
    try {
      await parquetRead(options);
    } catch (e) {
      if (e instanceof ParquetError) throw e;
      throw new ParquetError(e instanceof Error ? e.message : String(e));
    }
    return rows;
  }

  /** A column is flat when nothing on its path repeats: a v1 page's value
   *  count is then its row count, which is what the walk needs. */
  private isFlat(meta: ColumnMetaData): boolean {
    let node: SchemaTree | undefined = this.schema;
    for (const name of meta.path_in_schema) {
      node = node.children.find((c) => c.element.name === name);
      if (node === undefined || node.element.repetition_type === "REPEATED") return false;
    }
    return node !== undefined && node.children.length === 0;
  }

  /** Walk a chunk's page headers until they cover group row `to`; null when
   *  the headers can't be read this way (the caller then reads the chunk). */
  private walkTo(key: string, meta: ColumnMetaData, groupRows: number, to: number): Promise<PageLocation[] | null> {
    let walk = this.walks.get(key);
    if (walk === undefined) {
      // The footer is its metadata, its 4-byte length and `PAR1`.
      const span = chunkSpan(meta, this.file.byteLength - 8 - this.metadata.metadata_length);
      if (span === null) return Promise.resolve(null);
      walk = {
        pages: [],
        next: span.start,
        rows: 0,
        end: span.end,
        done: false,
        lock: Promise.resolve(),
      };
      this.walks.set(key, walk);
    }
    const w = walk;
    const run = w.lock.then(async () => {
      while (!walkCovers(w.rows, w.done, to)) {
        // The first header is read alone: it measures the round trip, and
        // its page's size says what the rest probably cost.
        const last = w.pages.at(-1)?.compressed_page_size ?? 0;
        const windowed = last > 0 && (last < TINY_PAGE || last < this.file.bdp);
        const header = await this.readHeader(w.next, w.end, windowed ? WALK_WINDOW : 0);
        if (header === null) return null;
        const total = header.length + header.compressed;
        if (header.dataRows !== null) {
          w.pages.push({ offset: BigInt(w.next), compressed_page_size: total, first_row_index: BigInt(w.rows) });
          w.rows += header.dataRows;
        }
        w.next += total;
        if (w.next >= w.end || w.rows >= groupRows) w.done = true;
      }
      return w.pages.length > 0 ? [...w.pages] : null;
    });
    w.lock = run.then(
      () => {},
      () => {},
    );
    return run.catch(() => null);
  }

  /** The page header at `at`, from cached bytes when a walk window holds it,
   *  else from a read of at least `prefer` bytes. */
  private async readHeader(
    at: number,
    end: number,
    prefer: number,
  ): Promise<{ length: number; compressed: number; dataRows: number | null } | null> {
    for (const win of HEADER_WINDOWS) {
      const buf =
        this.file.peek(at, Math.min(end, at + win)) ??
        (await this.file.slice(at, Math.min(end, at + Math.max(win, prefer))));
      const reader = { view: new DataView(buf), offset: 0 };
      let h: { [key: `field_${number}`]: unknown };
      try {
        h = deserializeTCompactProtocol(reader);
      } catch {
        if (at + win >= end) return null;
        continue;
      }
      if (reader.offset > buf.byteLength) {
        if (at + win >= end) return null;
        continue;
      }
      const type = h.field_1 as number;
      const compressed = h.field_3 as number;
      if (typeof compressed !== "number" || compressed < 0) return null;
      const v1 = h.field_5 as { field_1?: number } | undefined;
      const v2 = h.field_8 as { field_3?: number } | undefined;
      // 0 = DATA_PAGE, 3 = DATA_PAGE_V2; dictionary and index pages carry no rows.
      const dataRows = type === 0 ? (v1?.field_1 ?? null) : type === 3 ? (v2?.field_3 ?? null) : null;
      if ((type === 0 || type === 3) && dataRows === null) return null;
      return { length: reader.offset, compressed, dataRows };
    }
    return null;
  }
}

/** How a column's cells read, from its schema. */
function cellKind(node: SchemaTree): CellKind {
  const el = node.element;
  if (node.children.length > 0) return "value";
  if (el.logical_type?.type === "DATE" || el.converted_type === "DATE") return "date";
  if (el.type === "FLOAT") return "float32";
  return "value";
}

const UNIT = { MILLIS: "ms", MICROS: "us", NANOS: "ns" } as const;

/** One line per column: its logical (or physical) type and whether it may be null. */
export function describeType(node: SchemaTree): string {
  const el = node.element;
  const lt = el.logical_type;
  let type: string;
  if (lt !== undefined) {
    switch (lt.type) {
      case "DECIMAL":
        type = `decimal(${lt.precision}, ${lt.scale})`;
        break;
      case "TIMESTAMP":
        type = `timestamp[${UNIT[lt.unit]}${lt.isAdjustedToUTC ? ", UTC" : ""}]`;
        break;
      case "TIME":
        type = `time[${UNIT[lt.unit]}]`;
        break;
      case "INTEGER":
        type = `${lt.isSigned ? "int" : "uint"}${lt.bitWidth}`;
        break;
      default:
        type = lt.type.toLowerCase();
    }
  } else if (el.converted_type !== undefined) {
    type = el.converted_type.toLowerCase();
  } else if (el.type !== undefined) {
    type = el.type.toLowerCase();
  } else {
    type = node.children.length > 0 ? "group" : "?";
  }
  const rep = el.repetition_type === "REPEATED" ? "repeated" : el.repetition_type === "OPTIONAL" ? "nullable" : "";
  return rep === "" ? type : `${type} · ${rep}`;
}
