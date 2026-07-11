import test from 'node:test';
import assert from 'node:assert/strict';
import { connectState, postCommand, formatMs, formatLaps, phaseText } from '../src/lib/state.js';

class FakeEventSource {
	static last = null;
	constructor(url) {
		this.url = url;
		this.listeners = {};
		this.closed = false;
		FakeEventSource.last = this;
	}
	addEventListener(name, handler) {
		this.listeners[name] = handler;
	}
	emit(name, data) {
		this.listeners[name]?.({ data: JSON.stringify(data) });
	}
	close() {
		this.closed = true;
	}
}

const jsonResponse = (body, ok = true, status = 200) => ({
	ok,
	status,
	json: async () => body,
	text: async () => JSON.stringify(body)
});

test('connectState delivers initial fetch and strictly increasing stream states', async () => {
	const seen = [];
	const stop = connectState((state) => seen.push(state.sequence), null, {
		fetch: async () => jsonResponse({ sequence: 1, state: {} }),
		EventSource: FakeEventSource
	});
	const source = FakeEventSource.last;
	await Promise.resolve();
	await Promise.resolve();
	source.emit('state', { sequence: 1, state: {} }); // duplicate: dropped
	source.emit('state', { sequence: 3, state: {} });
	source.emit('state', { sequence: 2, state: {} }); // stale: dropped
	source.emit('state', { sequence: 4, state: {} });
	assert.deepEqual(seen, [1, 3, 4]);
	stop();
	assert.equal(source.closed, true);
	source.emit('state', { sequence: 9, state: {} });
	assert.deepEqual(seen, [1, 3, 4]);
});

test('connectState reports fetch failure and stream errors', async () => {
	const statuses = [];
	connectState(() => {}, (status) => statuses.push(status), {
		fetch: async () => jsonResponse({}, false, 503),
		EventSource: FakeEventSource
	});
	const source = FakeEventSource.last;
	source.onerror();
	await new Promise((resolve) => setTimeout(resolve, 0));
	assert.ok(statuses.includes('Verbindung unterbrochen'));
	assert.ok(statuses.includes('Zustand nicht erreichbar'));
});

test('postCommand posts JSON and returns state', async () => {
	let captured;
	const state = await postCommand('/api/pause', undefined, {
		fetch: async (path, init) => {
			captured = { path, init };
			return jsonResponse({ sequence: 5 });
		}
	});
	assert.equal(captured.path, '/api/pause');
	assert.equal(captured.init.method, 'POST');
	assert.equal(captured.init.body, '{}');
	assert.equal(state.sequence, 5);
});

test('postCommand surfaces server error text', async () => {
	await assert.rejects(
		postCommand('/api/start', { config: {} }, {
			fetch: async () => ({ ok: false, status: 400, text: async () => 'invalid duration' })
		}),
		/invalid duration/
	);
});

test('formatters render German-friendly values', () => {
	assert.equal(formatMs(null), '–');
	assert.equal(formatMs(61005), '1:01.005');
	assert.equal(formatLaps(2500), '2,5');
	assert.equal(formatLaps(undefined), '–');
});

test('phaseText names phases without color', () => {
	assert.equal(phaseText(null), 'Unbekannt');
	assert.equal(phaseText({ state: { status: 'ready' } }), 'Bereit');
	assert.equal(
		phaseText({ state: { status: 'active', control: 'live', lifecycle: 'running' } }),
		'Rennen läuft'
	);
	assert.equal(
		phaseText({ state: { status: 'active', control: 'paused', lifecycle: 'running' } }),
		'Rennpause'
	);
	assert.equal(phaseText({ state: { status: 'finished' } }), 'Beendet');
});
