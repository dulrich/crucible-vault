// Golden vectors for the cv-search chunker port (WP-13). Runs Crucible's own chunker.ts
// (read-only import) over SYNTHETIC inputs only and prints JSON. Strings are emitted as UTF-16
// code-unit arrays so lone surrogates survive the round trip.
// Usage: bun run docs/spikes/wp13-golden/golden.ts <obsidian-crucible checkout> (or set CRUCIBLE_CHECKOUT) > crates/cv-search/tests/fixtures/golden.json
const root = process.argv[2] ?? process.env.CRUCIBLE_CHECKOUT;
if (!root) {
  console.error('usage: golden.ts <obsidian-crucible checkout> (or set CRUCIBLE_CHECKOUT)');
  process.exit(2);
}
const m = await import(`${root}/src/search/chunker.ts`);
const { buildSearchChunks, hashSearchContent, stripFrontmatterBlock, parseSearchDocument } = m;

const u = (s: string) => Array.from({ length: s.length }, (_, i) => s.charCodeAt(i));
function fnv1a(value: string): string {
	let hash = 0x811c9dc5;
	for (let i = 0; i < value.length; i++) { hash ^= value.charCodeAt(i); hash = Math.imul(hash, 0x01000193); }
	return (hash >>> 0).toString(16).padStart(8, '0');
}

const emoji = '\u{1F600}';
const astralA = '\u{1D538}';
const cases: any[] = [];
function add(name: string, input: any) {
	const chunks = buildSearchChunks(input);
	const parsed = parseSearchDocument(input.content, input.basename);
	cases.push({
		name,
		input: {
			vaultId: input.vaultId, path: input.path, basename: input.basename, mtime: input.mtime,
			content: u(input.content), maxChars: input.maxChars, overlapChars: input.overlapChars,
			imageDescriptions: (input.imageDescriptions ?? []).map((d: any) => ({ filename: u(d.filename), narrative: u(d.narrative), extraction: u(d.extraction) })),
			linkedDocuments: (input.linkedDocuments ?? []).map((d: any) => ({ path: u(d.path), title: u(d.title), text: u(d.text) })),
			extraHashFacets: (input.extraHashFacets ?? []).map(u),
		},
		title: u(parsed.metadata.title),
		tags: parsed.metadata.tags.map(u),
		hash: hashSearchContent(input.content, input.extraHashFacets),
		chunks: chunks.map((c: any) => ({ id: c.id, contentHash: c.contentHash, heading: u(c.heading), text: u(c.text), ordinal: c.ordinal, entities: (c.entities ?? []).map((e: any) => u(e.text)) })),
	});
}
const base = { vaultId: 'golden-vault', path: 'notes/n.md', basename: 'n', mtime: 1700000000000, maxChars: 400, overlapChars: 0 };
const PROSE = ['---', 'title: Quarterly Report', 'author: Ada Lovelace', '---', '# Findings', 'Revenue rose.'].join('\n');
add('prose', { ...base, vaultId: 'test-vault', path: 'notes/report.md', basename: 'report', mtime: 7, content: PROSE });
add('alpha-beta', { ...base, vaultId: 'test', path: 'daily/example.md', basename: 'example', mtime: 1, maxChars: 80, overlapChars: 10, content: ['---', 'title: Example', '---', '# First', 'Alpha '.repeat(20), '', '## Second', 'Beta '.repeat(20)].join('\n') });
add('astral-headings', { ...base, path: `astral/${emoji}${astralA}.md`, basename: `${emoji}${astralA}`, content: ['---', `author: [${emoji} Writer, ${astralA}lice, "${emoji} writer"]`, 'tags: a, #b c', '---', `# ${astralA} Title ${emoji} ##`, `Body with ${emoji} and ${astralA}.`, `## ${emoji}`, 'Second.'].join('\n') });
// A paragraph of surrogate pairs longer than maxChars: sliceLongText splits mid-pair.
add('surrogate-split', { ...base, overlapChars: 7, content: ['# Split', 'x' + emoji.repeat(300)].join('\n') });
add('surrogate-split-overlap', { ...base, maxChars: 401, overlapChars: 133, content: emoji.repeat(150) + '\n\n' + 'y'.repeat(30) + astralA.repeat(200) });
add('cjk-combining', { ...base, maxChars: 400, overlapChars: 50, content: ['---', 'author:', '  - 山田太郎', '  - Zoë', '  - Zoë', '  - ΟΔΥΣΣΕΥΣ', '  - οδυσσευς', '---', '# 見出し', '日本語のテキスト。'.repeat(40), '', 'é́ combining '.repeat(30), '', '中文段落'.repeat(60)].join('\n') });
add('entity-cap', { ...base, content: ['---', `author: ${'a'.repeat(199)}${emoji} tail`, '---', 'body'].join('\n') });
add('entity-many', { ...base, content: ['---', 'author:', ...Array.from({ length: 40 }, (_, i) => `  - Person ${i % 35}`), '---', 'b'].join('\n') });
add('fm-backtrack', { ...base, content: '---\n\n---\nafter backtrack\n# H\ntext' });
add('fm-crlf', { ...base, content: '---\r\ntitle: Crlf\r\nauthor: "Q Writer"\r\n---\r\n# Head\r\nline one\r\nline two' });
add('fm-unclosed', { ...base, content: '---\ntitle: never closed\n# Real\nbody' });
add('fm-eof', { ...base, content: '---\nauthor: X\n---   ' });
add('fm-trailing-ws', { ...base, content: '---  \t\nauthor: Y\n---  \n  \nbody after' });
add('headings-edge', { ...base, content: ['#   ', '####### seven', '## trailing ##  ', '#\ttab', '# a b', '###### six', '#no-space', '# ', '##  #  '].join('\n') + '\nend' });
add('first-h1-multiline', { ...base, content: '#\nTitle From Next Line\nbody' });
add('first-h1-none', { ...base, content: 'no headings here\n## h2 only' });
add('fm-keys', { ...base, content: ['---', '2: two', '1: one', 'title: \'Quoted\'', 'tags: [x, "y", ]', '__proto__: z', 'created: 2024-01-01', 'a b: no', 'source: "', 'title: Again', '---', 'x'].join('\n') });
add('paragraph-packing', { ...base, overlapChars: 40, content: Array.from({ length: 30 }, (_, i) => `Paragraph ${i} ` + 'word '.repeat(10 + (i % 7) * 9)).join('\n\n') });
add('long-paragraph-overlap', { ...base, maxChars: 500, overlapChars: 300, content: 'z'.repeat(1700) });
add('images-linked', { ...base, content: PROSE,
	imageDescriptions: [
		{ filename: ' a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png ', narrative: `A chart ${emoji}.`, extraction: 'Row | 1\n\n'.repeat(80) },
		{ filename: 'diagram_MD5.svg', narrative: '', extraction: `Pipeline ${astralA}` },
		{ filename: '  ', narrative: 'orphan', extraction: 'orphan' },
	],
	linkedDocuments: Array.from({ length: 10 }, (_, i) => ({ path: `x/post${i}.md`, title: i === 2 ? '  ' : `post ${i} ${emoji}`, text: i === 3 ? '' : `Linked body ${i} ` + 'lorem '.repeat(i * 20) })),
	extraHashFacets: ['image-desc:deadbeef', 'linked:x/post0.md:12345678'],
});
// Sort order: code units (JS) put U+1F600 (D83D..) before U+E000; code points would not.
const sortFacets = [`linked::1`, `linked:${emoji}:1`, '  ', 'linked::1'];
add('facet-sort', { ...base, content: 'sorted facets', extraHashFacets: sortFacets });

const hashes = ['', 'a', 'abc', emoji, astralA, '\uD83D', '日本', `x\ny`].map(s => ({ input: u(s), hash: fnv1a(s) }));
// Linked-document preparation (filePreparation.ts:208,216).
const linkedRaw = `---\ntitle: meta\n---\n\n  ${'p'.repeat(3999)}${emoji}tail  `;
const linkedText = stripFrontmatterBlock(linkedRaw).trim().slice(0, 4000);
const linked = { raw: u(linkedRaw), text: u(linkedText), facetHash: hashSearchContent(linkedText) };
const combined = { lines: ['0123456789abcdef0123456789abcdef:1234abcd', 'fedcba9876543210fedcba9876543210:00000001'], hash: fnv1a(['0123456789abcdef0123456789abcdef:1234abcd', 'fedcba9876543210fedcba9876543210:00000001'].join('\n')) };
console.log(JSON.stringify({ generator: 'runs/spike/wp13-golden/golden.ts', cases, hashes, linked, combined }));
