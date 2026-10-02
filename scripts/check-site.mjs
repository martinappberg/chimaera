#!/usr/bin/env node
// A dependency-free guard for our hand-written static pages, not an HTML validator.
// Keep search identity and local navigation intact before CI publishes the site.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = resolve(dirname(fileURLToPath(import.meta.url)), '../site');
const origin = 'https://chimaera.sh';
const pages = [
  { file: 'index.html', url: `${origin}/` },
  { file: 'docs.html', url: `${origin}/docs.html` },
];
const titles = new Set();
const descriptions = new Set();
const read = (file) => readFileSync(join(site, file), 'utf8');
const attributes = (tag) => Object.fromEntries(
  [...tag.matchAll(/([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)]
    .map((match) => [match[1].toLowerCase(), match[2] ?? match[3]]),
);
const tags = (html, name) => [...html.matchAll(new RegExp(`<${name}\\b[^>]*>`, 'gi'))]
  .map((match) => attributes(match[0]));
const ids = (html) => [...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]);

for (const { file, url } of pages) {
  const html = read(file);
  const markup = html.replace(/<!--[\s\S]*?-->|<script\b[^>]*>[\s\S]*?<\/script>/gi, '');
  const head = html.match(/<head>([\s\S]*?)<\/head>/i)?.[1];
  assert(head, `${file}: missing head`);
  const meta = new Map();
  for (const tag of tags(head, 'meta')) {
    const key = tag.name ?? tag.property;
    if (!key) continue;
    assert(!meta.has(key), `${file}: duplicate ${key} metadata`);
    meta.set(key, tag.content);
  }
  const titleTags = [...head.matchAll(/<title>([^<]+)<\/title>/g)];
  assert.equal(titleTags.length, 1, `${file}: needs one title`);
  const title = titleTags[0][1].trim();
  const description = meta.get('description');
  assert(description?.trim(), `${file}: missing description`);
  assert(!titles.has(title), `${file}: title repeats another page`);
  assert(!descriptions.has(description), `${file}: description repeats another page`);
  titles.add(title);
  descriptions.add(description);
  assert.equal(tags(markup, 'html')[0]?.lang, 'en', `${file}: missing language`);
  assert.equal(tags(markup, 'h1').length, 1, `${file}: needs one static primary heading`);
  assert(meta.get('viewport')?.includes('width=device-width'), `${file}: missing mobile viewport`);
  assert(!/\b(noindex|nofollow|none)\b/i.test(meta.get('robots') ?? ''), `${file}: robots block indexing`);
  assert(meta.get('robots')?.includes('max-image-preview:large'), `${file}: large image previews disabled`);
  const canonical = tags(head, 'link').filter((tag) => tag.rel === 'canonical');
  assert.deepEqual(canonical.map((tag) => tag.href), [url], `${file}: wrong canonical`);
  assert.equal(meta.get('og:url'), url, `${file}: social URL differs from canonical`);
  for (const key of ['og:title', 'og:site_name', 'og:image:alt', 'twitter:title', 'twitter:image:alt']) {
    assert(meta.get(key)?.trim(), `${file}: missing ${key}`);
  }
  assert.equal(meta.get('og:description'), description, `${file}: social description drift`);
  assert.equal(meta.get('twitter:description'), description, `${file}: Twitter description drift`);
  assert.equal(meta.get('twitter:card'), 'summary_large_image', `${file}: small social card`);
  const imageUrl = new URL(meta.get('og:image'));
  assert.equal(imageUrl.origin, origin, `${file}: social image must be an absolute site URL`);
  assert.equal(meta.get('twitter:image'), imageUrl.href, `${file}: social images differ`);
  const png = readFileSync(join(site, imageUrl.pathname));
  assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a', `${file}: invalid PNG`);
  assert.equal(png.readUInt32BE(16), Number(meta.get('og:image:width')), `${file}: social image width drift`);
  assert.equal(png.readUInt32BE(20), Number(meta.get('og:image:height')), `${file}: social image height drift`);

  const dataTags = [...html.matchAll(/<script\b[^>]*type="application\/ld\+json"[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(dataTags.length, 1, `${file}: needs structured data`);
  const data = JSON.parse(dataTags[0][1]);
  assert.equal(data['@context'], 'https://schema.org', `${file}: invalid structured data context`);
  const page = data['@graph'].find((entry) => entry['@type'] === 'WebPage');
  assert.equal(page?.url, url, `${file}: structured page URL drift`);
  assert.equal(page?.name, title, `${file}: structured title drift`);
  assert.equal(page?.description, description, `${file}: structured description drift`);
  assert(data['@graph'].some((entry) => entry['@type'] === 'WebSite' && entry.url === `${origin}/`), `${file}: missing site identity`);

  const pageIds = ids(markup);
  assert.equal(new Set(pageIds).size, pageIds.length, `${file}: duplicate HTML IDs`);
  for (const tag of tags(markup, '(?:a|link|script|img|source|video|audio)')) {
    for (const href of [tag.href, tag.src, tag.poster].filter(Boolean)) {
      const target = new URL(href.replaceAll('&amp;', '&'), url);
      if (target.origin !== origin) continue;
      assert(!/\/index\.html(?:$|#)/.test(target.href), `${file}: link to canonical / instead of index.html`);
      const path = decodeURIComponent(target.pathname === '/' ? '/index.html' : target.pathname);
      assert(existsSync(join(site, path)), `${file}: missing local target ${href}`);
      if (target.hash && path.endsWith('.html')) {
        assert(ids(read(path)).includes(decodeURIComponent(target.hash.slice(1))), `${file}: missing anchor ${href}`);
      }
    }
  }
  // Script elements were removed above so inline JS cannot masquerade as links.
  for (const tag of tags(html, 'script').filter((tag) => tag.src)) {
    const target = new URL(tag.src, url);
    if (target.origin === origin) assert(existsSync(join(site, target.pathname)), `${file}: missing script ${tag.src}`);
  }
}

const sitemap = read('sitemap.xml');
assert(sitemap.includes('xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"'), 'sitemap: missing namespace');
const listed = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1]);
assert.deepEqual(listed.sort(), pages.map((page) => page.url).sort(), 'sitemap: canonical pages missing or duplicated');
const robots = read('robots.txt');
assert(/^User-agent: \*$/m.test(robots) && /^Allow: \/$/m.test(robots), 'robots: site must be crawlable');
assert(!/^Disallow:\s*\//mi.test(robots), 'robots: public pages or assets blocked');
assert(robots.includes(`Sitemap: ${origin}/sitemap.xml`), 'robots: missing sitemap');
assert(/<div\b[^>]*id="workspace-demo"[^>]*data-nosnippet/.test(read('index.html')), 'demo: sample content must be excluded from search snippets');
console.log(`✓ public site OK (${pages.length} pages, local links, search/sharing metadata, structured data, sitemap)`);
