import test from 'node:test';
import assert from 'node:assert/strict';
import {
	createStateClient,
	connectState,
	displayRaceElapsed,
	displayProtocolNow,
	currentLapMs,
	postCommand,
	formatMs,
	formatLaps,
	phaseText
} from '../src/lib/state.js';
import {
	startPayload,
	nextRacePayload,
	sensorPayload,
	raceChaosPayload,
	laneChaosPayload,
	correctionPayload
} from '../src/lib/control.js';

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

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const raceState = (sequence, fields = {}) => ({ race_id: 'race', race_generation: 0, sequence, state: {}, ...fields });

function client(states = [], statuses = []) {
	return createStateClient(
		(next) => {
			states.push(next.snapshot.sequence);
			statuses.push(next.connection);
		},
		{ now: () => 0 }
	);
}

test('client delivers initial fetch and strictly increasing stream states', async () => {
	const seen = [];
	const arbiter = createStateClient((next) => seen.push(next.snapshot?.sequence ?? null));
	const { stop } = connectState(arbiter, {
		fetch: async () => jsonResponse(raceState(1)),
		EventSource: FakeEventSource
	});
	const source = FakeEventSource.last;
	await settle();
	source.emit('state', raceState(1)); // duplicate: dropped
	source.emit('state', raceState(3));
	source.emit('state', raceState(2)); // stale: dropped
	source.emit('state', raceState(4));
	const sequences = seen.filter((sequence) => sequence !== null);
	assert.deepEqual([...new Set(sequences)], [1, 3, 4]);
	stop();
	assert.equal(source.closed, true);
	source.emit('state', raceState(9));
	assert.deepEqual([...new Set(seen.filter((sequence) => sequence !== null))], [1, 3, 4]);
});

test('same-sequence SSE refresh re-anchors volatile clocks after reconnect', () => {
	let now = 100;
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next), { now: () => now });
	assert.equal(arbiter.accept(raceState(4, { protocol_now: 1_000 })), true);
	now = 500;
	assert.equal(arbiter.accept(raceState(4, { protocol_now: 1_000 })), false);
	assert.equal(latest.receivedAt, 100);
	assert.equal(arbiter.accept(raceState(4, { protocol_now: 1_000 }), true), true);
	assert.equal(latest.receivedAt, 500);
});

test('delayed same-sequence SSE cannot regress a newer fetch envelope', async () => {
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next));
	let resolveFetch;
	connectState(arbiter, {
		fetch: () => new Promise((resolve) => (resolveFetch = resolve)),
		EventSource: FakeEventSource
	});
	const source = FakeEventSource.last;
	source.onopen();
	resolveFetch(jsonResponse(raceState(7, { protocol_now: 2_000 })));
	await settle();
	source.emit('state', raceState(7, { protocol_now: 1_000 }));
	assert.equal(latest.snapshot.protocol_now, 2_000);
});

test('POST after reconnect consumes refresh before delayed same-sequence SSE', async () => {
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next));
	const stream = connectState(arbiter, {
		fetch: async () => jsonResponse(raceState(7, { protocol_now: 1_000 })),
		EventSource: FakeEventSource
	});
	await settle();
	const source = FakeEventSource.last;
	source.onopen(); // reconnect grants one same-sequence refresh
	stream.accept(raceState(8, { protocol_now: 2_000 }));
	source.emit('state', raceState(8, { protocol_now: 1_500 }));
	assert.equal(latest.snapshot.protocol_now, 2_000);
});

test('fetch failure after a successful SSE does not overwrite connected status', async () => {
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next));
	let failFetch;
	const fetchPromise = new Promise((_, reject) => (failFetch = reject));
	connectState(arbiter, {
		fetch: () => fetchPromise,
		EventSource: FakeEventSource
	});
	const source = FakeEventSource.last;
	source.emit('state', raceState(1)); // SSE connects first
	assert.equal(latest.connection, 'verbunden');
	assert.equal(latest.connected, true);
	failFetch(new Error('late failure'));
	await settle();
	assert.equal(latest.connection, 'verbunden');
	assert.equal(latest.connected, true);
});

test('fetch failure without SSE reports unreachable', async () => {
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next));
	connectState(arbiter, {
		fetch: async () => jsonResponse({}, false, 503),
		EventSource: FakeEventSource
	});
	FakeEventSource.last.onerror();
	assert.equal(latest.connection, 'Verbindung unterbrochen');
	await settle();
	assert.equal(latest.connection, 'Zustand nicht erreichbar');
	assert.equal(latest.connected, false);
});

test('POST results feed the same arbiter: stale responses are dropped', async () => {
	const seen = [];
	const arbiter = createStateClient((next) => seen.push(next.snapshot.sequence));
	const { accept } = connectState(arbiter, {
		fetch: async () => jsonResponse(raceState(5)),
		EventSource: FakeEventSource
	});
	await settle();
	assert.equal(accept(raceState(4)), false); // stale POST result
	assert.equal(accept(raceState(6)), true);
	assert.deepEqual(seen, [5, 6]);
});

test('generation orders race switches without terminal or ready boundary heuristics', () => {
	const seen = [];
	const arbiter = createStateClient((next) =>
		seen.push(`${next.snapshot.race_generation}:${next.snapshot.race_id}:${next.snapshot.sequence}`)
	);
	assert.equal(arbiter.accept({ race_id: 'old', race_generation: 4, sequence: 8, state: { status: 'active' } }), true);
	assert.equal(arbiter.accept({ race_id: 'new', race_generation: 5, sequence: 3, state: { status: 'active' } }), true);
	assert.equal(arbiter.accept({ race_id: 'old', race_generation: 4, sequence: 99, state: { status: 'finished' } }, true), false);
	assert.equal(arbiter.accept({ race_id: 'wrong', race_generation: 5, sequence: 4, state: {} }), false);
	assert.equal(arbiter.accept({ race_id: 'new', race_generation: 5, sequence: 4, state: {} }), true);
	// A greater generation is authoritative even if its race ID was seen before.
	assert.equal(arbiter.accept({ race_id: 'old', race_generation: 6, sequence: 0, state: { status: 'ready' } }), true);
	assert.deepEqual(seen, ['4:old:8', '5:new:3', '5:new:4', '6:old:0']);
});

test('reconnect accepts a greater generation after the switch boundary was missed', async () => {
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next));
	connectState(arbiter, {
		fetch: async () =>
			jsonResponse({ race_id: 'old', race_generation: 7, sequence: 8, state: { status: 'active' } }),
		EventSource: FakeEventSource
	});
	await settle();
	const source = FakeEventSource.last;
	source.onopen();
	source.emit('state', { race_id: 'new', race_generation: 8, sequence: 3, state: { status: 'active' } });
	assert.equal(latest.snapshot.race_id, 'new');
	assert.equal(latest.snapshot.race_generation, 8);
});

test('delayed lower-generation fetch and POST responses cannot regress state', async () => {
	let latest = null;
	let resolveFetch;
	const arbiter = createStateClient((next) => (latest = next));
	const stream = connectState(arbiter, {
		fetch: () => new Promise((resolve) => (resolveFetch = resolve)),
		EventSource: FakeEventSource
	});
	FakeEventSource.last.emit('state', { race_id: 'new', race_generation: 2, sequence: 1, state: {} });
	assert.equal(stream.accept({ race_id: 'old', race_generation: 1, sequence: 100, state: {} }), false);
	resolveFetch(jsonResponse({ race_id: 'old', race_generation: 1, sequence: 100, state: {} }));
	await settle();
	assert.equal(latest.snapshot.race_id, 'new');
	assert.equal(latest.snapshot.race_generation, 2);
});

test('client uses injected monotonic time for receivedAt', () => {
	let tick = 100;
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next), { now: () => tick });
	arbiter.accept(raceState(1));
	assert.equal(latest.receivedAt, 100);
	tick = 250;
	arbiter.accept(raceState(2));
	assert.equal(latest.receivedAt, 250);
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

test('race clock advances only when server says running and connection is live', () => {
	const running = { race_elapsed_ms: 500, race_clock_running: true };
	assert.equal(displayRaceElapsed(running, 1_000, true, 1_250), 750);
	// Frozen while disconnected: no extrapolation.
	assert.equal(displayRaceElapsed(running, 1_000, false, 1_250), 500);
	// Frozen when the server clock is not running (paused, starting, finished).
	assert.equal(
		displayRaceElapsed({ race_elapsed_ms: 500, race_clock_running: false }, 1_000, true, 1_250),
		500
	);
	assert.equal(displayRaceElapsed({ race_elapsed_ms: null }, 1_000, true, 1_250), null);
	// Monotonic regression never rolls the clock back.
	assert.equal(displayRaceElapsed(running, 1_000, true, 900), 500);
});

test('disconnect freezes clocks at their last monotonic value without rewinding', () => {
	let now = 100;
	let latest = null;
	const arbiter = createStateClient((next) => (latest = next), { now: () => now });
	arbiter.accept(raceState(1, { protocol_now: 10_000, race_elapsed_ms: 500, race_clock_running: true }));
	arbiter.setStatus('verbunden', true);
	now = 350;
	arbiter.setStatus('Verbindung unterbrochen', false);
	assert.equal(latest.disconnectedAt, 350);
	assert.equal(displayProtocolNow(latest.snapshot, latest.receivedAt, false, 1_000, latest.disconnectedAt), 10_250);
	assert.equal(displayRaceElapsed(latest.snapshot, latest.receivedAt, false, 1_000, latest.disconnectedAt), 750);
});

test('displayProtocolNow projects protocol time by monotonic delta while connected', () => {
	const snapshot = { protocol_now: 10_000 };
	assert.equal(displayProtocolNow(snapshot, 1_000, true, 1_400), 10_400);
	assert.equal(displayProtocolNow(snapshot, 1_000, false, 1_400), 10_000);
	assert.equal(displayProtocolNow({}, 1_000, true, 1_400), null);
});

test('currentLapMs anchors on last valid pass, else official start', () => {
	const snapshot = {
		race_clock_running: true,
		state: { status: 'active', lifecycle: 'running', official_start_at: 5_000 }
	};
	assert.equal(currentLapMs(snapshot, { last_valid_at: 8_000 }, 9_500), 1_500);
	assert.equal(currentLapMs(snapshot, { last_valid_at: null }, 9_500), 4_500);
	assert.equal(
		currentLapMs({ ...snapshot, race_clock_running: false }, { last_valid_at: 8_000 }, 9_500),
		1_500
	); // Messereignisse still use protocol time during Rennpause.
	assert.equal(currentLapMs(snapshot, { last_valid_at: null }, null), null);
	assert.equal(currentLapMs(snapshot, { last_valid_at: 8_000, finished_at: 9_000 }, 9_500), null);
	assert.equal(currentLapMs({ state: { status: 'finished' } }, {}, 9_500), null);
});

test('control payload builders produce the exact API bodies', () => {
	assert.deepEqual(
		startPayload({
			lanes: '2',
			driverIds: ['7', ''],
			startSequenceMs: '3000',
			restartSequenceMs: '2000',
			minimumLapTimeMs: '1500',
			finishKind: 'laps',
			finishLaps: '10',
			finishTimeMs: '300000',
			finishMode: 'immediate',
			falseStartKind: 'abort',
			falseStartMs: '5000',
			chaosKind: 'result_time_penalty_ms',
			chaosMs: '2500'
		}),
		{
			config: {
				lanes: 2,
				driver_ids: [7, null],
				start_sequence_ms: 3000,
				restart_sequence_ms: 2000,
				minimum_lap_time_ms: 1500,
				finish_condition: { laps: 10 },
				finish_mode: 'immediate',
				false_start_consequence: 'abort',
				chaos_consequence: { result_time_penalty_ms: 2500 }
			}
		}
	);
	assert.deepEqual(
		startPayload({
			lanes: 4,
			driverIds: ['', '', '', ''],
			startSequenceMs: 1,
			restartSequenceMs: 1,
			minimumLapTimeMs: 1,
			finishKind: 'time_ms',
			finishLaps: 10,
			finishTimeMs: 60000,
			finishMode: 'leader_lap',
			falseStartKind: 'lane_power_off_ms',
			falseStartMs: 4000,
			chaosKind: 'abort',
			chaosMs: 1
		}).config.finish_condition,
		{ time_ms: 60000 }
	);
	assert.deepEqual(nextRacePayload('race-1', 'race-2'), {
		expected_race_id: 'race-1',
		next_race_id: 'race-2'
	});
	assert.deepEqual(sensorPayload('2', 'rising'), { lane: 2, edge: 'rising' });
	assert.deepEqual(raceChaosPayload(), { source: 'race_control' });
	assert.deepEqual(laneChaosPayload('3'), { source: { lane: 3 } });
	assert.deepEqual(correctionPayload('1', '0,5'), { lane: 1, delta_thousandths: 500 });
	assert.deepEqual(correctionPayload(2, '-1,25'), { lane: 2, delta_thousandths: -1250 });
});

test('formatters render German-friendly values', () => {
	assert.equal(formatMs(null), '–');
	assert.equal(formatMs(61005), '1:01.005');
	assert.equal(formatMs(750.25), '0:00.750');
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
