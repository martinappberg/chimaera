/** Rasterize the editable social card. Requires Sharp (npm install sharp).
 * Run: node site/assets/img/og-workspace-2026-10.mjs
 * A bundled Sharp install may be passed with CHIMAERA_SHARP_MODULE=/absolute/path/to/sharp.
 * SVG text uses Arial/Helvetica and Menlo/DejaVu Sans Mono; install those for matching typography.
 */
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
const require = createRequire(import.meta.url);
const sharp = require(process.env.CHIMAERA_SHARP_MODULE || 'sharp');
const input = fileURLToPath(new URL('./og-workspace-2026-10.svg', import.meta.url));
const output = fileURLToPath(new URL('./og-workspace-2026-10.png', import.meta.url));
await sharp(input, { density: 192 })
  .resize(1200, 630)
  .flatten({ background: '#fbfbfc' })
  .png({ compressionLevel: 9 })
  .toFile(output);
const metadata = await sharp(output).metadata();
console.log(`${output}: ${metadata.width} × ${metadata.height}`);
