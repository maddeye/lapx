// Shared state client for Rennscreen and Rennleiter control.
// Injected globals keep this testable under node:test.

/**
 * Single arbiter for server state: owns the sequence cursor, snapshot
 * acceptance, connection status, and the monotonic receive timestamp.
 * All snapshot sources (initial fetch, SSE, POST responses) go through
 * accept(); nothing assigns snapshots directly.
 */
export function createStateClient(onChange, deps = {}) {
	const now = deps.now ?? (() => performance.now());
	let sequence = -1;
	let snapshot = null;
	let connection = 'verbinde …';
	let receivedAt = 0;
	let connected = false;
	let disconnectedAt = null;

	const emit = () => onChange({ snapshot, connection, connected, receivedAt, disconnectedAt });

	return {
		now,
		accept(state, refresh = false) {
			if (
				typeof state?.sequence !== 'number' ||
				state.sequence < sequence ||
				(state.sequence === sequence && !refresh)
			) return false;
			sequence = state.sequence;
			snapshot = state;
			receivedAt = now();
			emit();
			return true;
		},
		setStatus(status, isConnected) {
			// A late fetch failure must not mask a live SSE connection.
			if (!isConnected && connected && status === 'Zustand nicht erreichbar') return;
			if (connected && !isConnected) disconnectedAt = now();
			if (isConnected) disconnectedAt = null;
			connection = status;
			connected = isConnected;
			emit();
		},
		get connected() {
			return connected;
		}
	};
}

/**
 * Subscribes client to /api/state + /api/state/stream.
 * Returns { accept, stop }: accept feeds POST responses through the same
 * arbiter, stop closes the stream.
 */
export function connectState(client, deps = {}) {
	const fetchFn = deps.fetch ?? globalThis.fetch;
	const EventSourceCtor = deps.EventSource ?? globalThis.EventSource;
	let stopped = false;
	let refreshNext = false;

	const source = new EventSourceCtor('/api/state/stream');
	source.addEventListener('state', (event) => {
		if (stopped) return;
		try {
			client.accept(JSON.parse(event.data), refreshNext);
			refreshNext = false;
			client.setStatus('verbunden', true);
		} catch {
			client.setStatus('fehlerhafte Daten', false);
		}
	});
	source.onopen = () => {
		if (stopped) return;
		refreshNext = true;
		client.setStatus('verbunden', true);
	};
	// EventSource reconnects on its own; only report the gap.
	source.onerror = () => stopped || client.setStatus('Verbindung unterbrochen', false);

	fetchFn('/api/state')
		.then((response) => {
			if (!response.ok) throw new Error(String(response.status));
			return response.json();
		})
		.then((state) => {
			if (!stopped && client.accept(state)) refreshNext = false;
		})
		.catch(() => stopped || client.setStatus('Zustand nicht erreichbar', false));

	return {
		accept: (state) => {
			const accepted = client.accept(state);
			if (accepted) refreshNext = false;
			return accepted;
		},
		stop: () => {
			stopped = true;
			source.close();
		}
	};
}

/** POSTs a command body to an /api route; resolves the new state or throws the server error text. */
export async function postCommand(path, body, deps = {}) {
	const fetchFn = deps.fetch ?? globalThis.fetch;
	const response = await fetchFn(path, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body ?? {})
	});
	if (!response.ok) {
		throw new Error((await response.text()) || `Fehler ${response.status}`);
	}
	return response.json();
}

/**
 * Server protocol clock projected to display time: protocol_now base plus
 * the monotonic delta since the snapshot arrived. Frozen while disconnected.
 */
export function displayProtocolNow(snapshot, receivedAt, connected, monotonicNow, disconnectedAt = null) {
	const base = snapshot?.protocol_now;
	if (base === null || base === undefined) return null;
	const displayAt = connected ? monotonicNow : disconnectedAt;
	if (displayAt === null) return base;
	return base + Math.max(0, displayAt - receivedAt);
}

/**
 * Race clock for display: advances only while the server says the race
 * clock runs and the connection is live. Extrapolates via monotonic time.
 */
export function displayRaceElapsed(snapshot, receivedAt, connected, monotonicNow, disconnectedAt = null) {
	const base = snapshot?.race_elapsed_ms;
	if (base === null || base === undefined) return base;
	const displayAt = connected ? monotonicNow : disconnectedAt;
	if (!snapshot.race_clock_running || displayAt === null) return base;
	return base + Math.max(0, displayAt - receivedAt);
}

/**
 * Milliseconds into the current lap for a lane, from the lap anchor
 * (last valid pass, else official start) to projected protocol time.
 * Null when the race clock is not running or no anchor exists.
 */
export function currentLapMs(snapshot, lane, protocolNow) {
	const race = snapshot?.state;
	const lapActive =
		race?.status === 'active' && (race.lifecycle === 'running' || race.lifecycle === 'finishing');
	const laneFinished = lane?.finished_at !== null && lane?.finished_at !== undefined;
	if (!lapActive || laneFinished || protocolNow === null || protocolNow === undefined) return null;
	const anchor = lane?.last_valid_at ?? race.official_start_at;
	if (anchor === null || anchor === undefined) return null;
	return Math.max(0, protocolNow - anchor);
}

export function formatMs(ms) {
	if (ms === null || ms === undefined) return '–';
	const total = Math.max(0, Math.round(ms));
	const minutes = Math.floor(total / 60000);
	const seconds = Math.floor((total % 60000) / 1000);
	const millis = total % 1000;
	return `${minutes}:${String(seconds).padStart(2, '0')}.${String(millis).padStart(3, '0')}`;
}

/** Formats corrected laps (thousandths) as a decimal lap count. */
export function formatLaps(thousandths) {
	if (thousandths === null || thousandths === undefined) return '–';
	return (thousandths / 1000).toLocaleString('de-DE', {
		minimumFractionDigits: 0,
		maximumFractionDigits: 3
	});
}

const PHASES = {
	ready: 'Bereit',
	active: 'Aktiv',
	finished: 'Beendet',
	aborted: 'Abgebrochen'
};

const LIFECYCLES = {
	starting: 'Startsequenz',
	running: 'Rennen läuft',
	finishing: 'Zieldurchfahrt'
};

const CONTROLS = {
	paused: 'Rennpause',
	restarting: 'Wiederanlaufsequenz'
};

/** Human phase text for a state snapshot; German, never color-only. */
export function phaseText(state) {
	if (!state?.state) return 'Unbekannt';
	const race = state.state;
	if (race.status !== 'active') return PHASES[race.status] ?? race.status;
	if (race.control && race.control !== 'live') return CONTROLS[race.control] ?? race.control;
	return LIFECYCLES[race.lifecycle] ?? PHASES.active;
}
