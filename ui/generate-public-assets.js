import fs from 'node:fs';
import path from 'node:path';

const out = process.env.LAPX_UI_OUT ?? 'build';
const html = fs.readFileSync(path.join(out, 'index.html'), 'utf8');
const assets = new Set(['_app/version.json']);
for (const match of html.matchAll(/(?:href|src)="\.\/(_app\/[^"]+)"/g)) assets.add(match[1]);
fs.writeFileSync(
	path.join(out, 'public-assets.json'),
	`${JSON.stringify([...assets].sort(), null, 2)}\n`
);
