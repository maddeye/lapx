// Byte-compares a freshly built UI against the committed build/ and fails on
// stale artifacts. Never modifies the canonical build.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const root = path.dirname(new URL(import.meta.url).pathname);
const canonical = path.join(root, 'build');
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'lapx-ui-build-'));

function listFiles(dir, base = dir) {
	return fs
		.readdirSync(dir, { withFileTypes: true })
		.flatMap((entry) => {
			const full = path.join(dir, entry.name);
			return entry.isDirectory() ? listFiles(full, base) : [path.relative(base, full)];
		})
		.sort();
}

try {
	execFileSync('npm', ['run', 'build'], {
		cwd: root,
		stdio: 'inherit',
		env: { ...process.env, LAPX_UI_OUT: temp }
	});
	const expected = listFiles(canonical);
	const actual = listFiles(temp);
	if (expected.join('\n') !== actual.join('\n')) {
		console.error('stale ui/build: file sets differ');
		console.error(`committed:\n${expected.join('\n')}\nrebuilt:\n${actual.join('\n')}`);
		process.exit(1);
	}
	for (const file of expected) {
		const left = fs.readFileSync(path.join(canonical, file));
		const right = fs.readFileSync(path.join(temp, file));
		if (!left.equals(right)) {
			console.error(`stale ui/build: ${file} differs; run \`npm run build\` and commit`);
			process.exit(1);
		}
	}
	console.log(`ui/build is up to date (${expected.length} files)`);
} finally {
	fs.rmSync(temp, { recursive: true, force: true });
}
