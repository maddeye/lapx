// Shared state client for Rennscreen and Rennleiter control.
// Injected globals keep this testable under node:test.

/**
 * Subscribes to /api/state + /api/state/stream and forwards strictly
 * newer snapshots to onState. Returns a stop() cleanup function.
 */
export function connectState(onState, onStatus, deps = {}) {
	const fetchFn = deps.fetch ?? globalThis.fetch;
	const EventSourceCtor = deps.EventSource ?? globalThis.EventSource;
	let sequence = -1;
	let stopped = false;

	const accept = (state) => {
		if (stopped || typeof state?.sequence !== 'number' || state.sequence <= sequence) return;
		sequence = state.sequence;
		onState(state);
	};

	const source = new EventSourceCtor('/api/state/stream');
	source.addEventListener('state', (event) => {
		try {
			accept(JSON.parse(event.data));
			onStatus?.('verbunden');
		} catch {
			onStatus?.('fehlerhafte Daten');
		}
	});
	source.onopen = () => onStatus?.('verbunden');
	// EventSource reconnects on its own; only report the gap.
	source.onerror = () => onStatus?.('Verbindung unterbrochen');

	fetchFn('/api/state')
		.then((response) => {
			if (!response.ok) throw new Error(String(response.status));
			return response.json();
		})
		.then(accept)
		.catch(() => onStatus?.('Zustand nicht erreichbar'));

	return () => {
		stopped = true;
		source.close();
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

/** Formats protocol milliseconds as m:ss.mmm for display. */
export function formatMs(ms) {
	if (ms === null || ms === undefined) return '–';
	const minutes = Math.floor(ms / 60000);
	const seconds = Math.floor((ms % 60000) / 1000);
	const millis = ms % 1000;
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
