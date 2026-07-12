// Rendered smoke test: compile both pages through Vite SSR and assert the
// actual control markup (forms, buttons, labels) without a browser.
import test from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.dirname(fileURLToPath(import.meta.url));

async function renderPage(relative, props = {}) {
	const { createServer } = await import('vite');
	const server = await createServer({
		root: path.join(root, '..'),
		logLevel: 'error',
		server: { middlewareMode: true, hmr: false, ws: false },
		optimizeDeps: { noDiscovery: true }
	});
	try {
		// Same module graph for component and renderer; mixing instances breaks dev SSR.
		const { render } = await server.ssrLoadModule('svelte/server');
		const module = await server.ssrLoadModule(relative);
		return render(module.default, { props }).body;
	} finally {
		await server.close();
	}
}

test('control page renders every Rennleiter control and no Advance', async () => {
	const body = await renderPage('/src/routes/control/+page.svelte');
	assert.match(body, /<h1[^>]*>Rennleitung<\/h1>/);
	assert.match(body, /<form/);
	for (const heading of [
		'Rennkonfiguration',
		'Rennsteuerung',
		'Messereignis simulieren',
		'Rundenkorrektur',
		'Rennstand'
	]) {
		assert.ok(body.includes(heading), `missing section ${heading}`);
	}
	for (const button of [
		'Rennen starten',
		'Rennpause',
		'Wiederanlauf',
		'Chaos Rennleitung',
		'Chaos Bahn auslösen',
		'Messereignis senden',
		'Korrektur anwenden'
	]) {
		assert.ok(body.includes(button), `missing button ${button}`);
	}
	for (const label of [
		'Bahnen',
		'Startsequenz (ms)',
		'Mindestrundenzeit (ms)',
		'Zielbedingung',
		'Fehlstartfolge',
		'Chaosfolge',
		'Flanke'
	]) {
		assert.ok(body.includes(label), `missing label ${label}`);
	}
	// Advance is protocol-internal; the Rennleiter must never see it.
	assert.ok(!/advance/i.test(body), 'control page must not expose Advance');
	// Aktuelle Runde column from LaneTable.
	assert.ok(body.includes('Aktuelle Runde'));
});

test('admin page renders Fahrer create, rename, archive controls', async () => {
	const body = await renderPage('/src/routes/admin/+page.svelte', {
		initialDrivers: [{ id: 7, display_name: 'Ada', archived_at: null }]
	});
	assert.match(body, /<h1[^>]*>Fahrer<\/h1>/);
	for (const text of [
		'Fahrer anlegen',
		'Anzeigename',
		'Anlegen',
		'Fahrerliste',
		'Ada',
		'Umbenennen',
		'Archivieren'
	]) {
		assert.ok(body.includes(text), `missing admin control ${text}`);
	}
	assert.ok(body.includes('<form'));
	assert.ok(!body.includes('Löschen'), 'admin must not expose hard delete');
});

test('rennscreen renders the public display without controls', async () => {
	const body = await renderPage('/src/routes/+page.svelte');
	assert.match(body, /<h1[^>]*>Rennscreen<\/h1>/);
	assert.ok(body.includes('Aktuelle Runde'));
	assert.ok(!body.includes('<form'), 'Rennscreen has no forms');
	assert.ok(!body.includes('<button'), 'Rennscreen has no buttons');
});
