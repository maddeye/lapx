// Pure payload builders for the Rennleiter control page; testable under node:test.

function consequence(kind, ms) {
	return kind === 'abort' ? 'abort' : { [kind]: Number(ms) };
}

export function startPayload(form) {
	return {
		config: {
			lanes: Number(form.lanes),
			start_sequence_ms: Number(form.startSequenceMs),
			restart_sequence_ms: Number(form.restartSequenceMs),
			minimum_lap_time_ms: Number(form.minimumLapTimeMs),
			finish_condition:
				form.finishKind === 'laps'
					? { laps: Number(form.finishLaps) }
					: { time_ms: Number(form.finishTimeMs) },
			finish_mode: form.finishMode,
			false_start_consequence: consequence(form.falseStartKind, form.falseStartMs),
			chaos_consequence: consequence(form.chaosKind, form.chaosMs)
		}
	};
}

export function sensorPayload(lane, edge) {
	return { lane: Number(lane), edge };
}

export function raceChaosPayload() {
	return { source: 'race_control' };
}

export function laneChaosPayload(lane) {
	return { source: { lane: Number(lane) } };
}

export function correctionPayload(lane, laps) {
	return {
		lane: Number(lane),
		delta_thousandths: Math.round(Number(String(laps).replace(',', '.')) * 1000)
	};
}
