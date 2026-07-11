<script>
	import { onMount } from 'svelte';
	import { connectState, postCommand, formatMs, phaseText } from '$lib/state.js';
	import LaneTable from '$lib/LaneTable.svelte';

	let snapshot = $state(null);
	let connection = $state('verbinde …');
	let error = $state('');

	let lanes = $state(2);
	let startSequenceMs = $state(3000);
	let restartSequenceMs = $state(3000);
	let minimumLapTimeMs = $state(3000);
	let finishKind = $state('laps');
	let finishLaps = $state(10);
	let finishTimeMs = $state(300000);
	let finishMode = $state('immediate');
	let falseStartKind = $state('abort');
	let falseStartMs = $state(5000);
	let chaosKind = $state('abort');
	let chaosMs = $state(5000);

	let sensorLane = $state(1);
	let sensorEdge = $state('rising');
	let chaosLane = $state(1);
	let correctionLane = $state(1);
	let correctionLaps = $state('0');

	onMount(() =>
		connectState(
			(next) => (snapshot = next),
			(status) => (connection = status)
		)
	);

	function consequence(kind, ms, key) {
		return kind === 'abort' ? 'abort' : { [key]: Number(ms) };
	}

	async function run(action) {
		error = '';
		try {
			snapshot = await action();
		} catch (failure) {
			error = failure.message;
		}
	}

	const start = () =>
		run(() =>
			postCommand('/api/start', {
				config: {
					lanes: Number(lanes),
					start_sequence_ms: Number(startSequenceMs),
					restart_sequence_ms: Number(restartSequenceMs),
					minimum_lap_time_ms: Number(minimumLapTimeMs),
					finish_condition:
						finishKind === 'laps'
							? { laps: Number(finishLaps) }
							: { time_ms: Number(finishTimeMs) },
					finish_mode: finishMode,
					false_start_consequence:
						falseStartKind === 'abort'
							? 'abort'
							: consequence(falseStartKind, falseStartMs, falseStartKind),
					chaos_consequence:
						chaosKind === 'abort' ? 'abort' : consequence(chaosKind, chaosMs, chaosKind)
				}
			})
		);
	const pause = () => run(() => postCommand('/api/pause'));
	const resume = () => run(() => postCommand('/api/resume'));
	const raceChaos = () => run(() => postCommand('/api/chaos', { source: 'race_control' }));
	const laneChaos = () =>
		run(() => postCommand('/api/chaos', { source: { lane: Number(chaosLane) } }));
	const sensor = () =>
		run(() => postCommand('/api/sensor', { lane: Number(sensorLane), edge: sensorEdge }));
	const correct = () =>
		run(() =>
			postCommand('/api/correct-laps', {
				lane: Number(correctionLane),
				delta_thousandths: Math.round(Number(correctionLaps.replace(',', '.')) * 1000)
			})
		);
</script>

<svelte:head>
	<title>LapX Rennleitung</title>
</svelte:head>

<main>
	<h1>Rennleitung</h1>
	<p aria-live="polite">
		<span>Phase: {phaseText(snapshot)}</span>
		<span>Rennzeit: {formatMs(snapshot?.race_elapsed_ms)}</span>
		<span>Verbindung: {connection}</span>
	</p>
	{#if error}
		<p class="error" role="alert">Fehler: {error}</p>
	{/if}

	<section aria-labelledby="config-heading">
		<h2 id="config-heading">Rennkonfiguration</h2>
		<form onsubmit={(event) => (event.preventDefault(), start())}>
			<div class="grid">
				<label>Bahnen
					<input type="number" min="1" max="4" bind:value={lanes} />
				</label>
				<label>Startsequenz (ms)
					<input type="number" min="1" bind:value={startSequenceMs} />
				</label>
				<label>Wiederanlaufsequenz (ms)
					<input type="number" min="1" bind:value={restartSequenceMs} />
				</label>
				<label>Mindestrundenzeit (ms)
					<input type="number" min="1" bind:value={minimumLapTimeMs} />
				</label>
				<label>Zielbedingung
					<select bind:value={finishKind}>
						<option value="laps">Runden</option>
						<option value="time_ms">Zeit (ms)</option>
					</select>
				</label>
				{#if finishKind === 'laps'}
					<label>Zielrunden
						<input type="number" min="1" bind:value={finishLaps} />
					</label>
				{:else}
					<label>Zielzeit (ms)
						<input type="number" min="1" bind:value={finishTimeMs} />
					</label>
				{/if}
				<label>Zielmodus
					<select bind:value={finishMode}>
						<option value="immediate">Sofort</option>
						<option value="leader_lap">Führungsrunde</option>
						<option value="all_current_lap">Alle aktuelle Runde</option>
					</select>
				</label>
				<label>Fehlstartfolge
					<select bind:value={falseStartKind}>
						<option value="abort">Abbruch</option>
						<option value="result_time_penalty_ms">Zeitstrafe</option>
						<option value="lane_power_off_ms">Bahn stromlos</option>
					</select>
				</label>
				{#if falseStartKind !== 'abort'}
					<label>Fehlstart-Dauer (ms)
						<input type="number" min="1" bind:value={falseStartMs} />
					</label>
				{/if}
				<label>Chaosfolge
					<select bind:value={chaosKind}>
						<option value="abort">Abbruch</option>
						<option value="result_time_penalty_ms">Zeitstrafe</option>
						<option value="lane_power_off_ms">Bahn stromlos</option>
					</select>
				</label>
				{#if chaosKind !== 'abort'}
					<label>Chaos-Dauer (ms)
						<input type="number" min="1" bind:value={chaosMs} />
					</label>
				{/if}
			</div>
			<button type="submit">Rennen starten</button>
		</form>
	</section>

	<section aria-labelledby="control-heading">
		<h2 id="control-heading">Rennsteuerung</h2>
		<div class="row">
			<button type="button" onclick={pause}>Rennpause</button>
			<button type="button" onclick={resume}>Wiederanlauf</button>
			<button type="button" onclick={raceChaos}>Chaos Rennleitung</button>
		</div>
		<div class="row">
			<label>Chaos-Bahn
				<input type="number" min="1" max="4" bind:value={chaosLane} />
			</label>
			<button type="button" onclick={laneChaos}>Chaos Bahn auslösen</button>
		</div>
	</section>

	<section aria-labelledby="sensor-heading">
		<h2 id="sensor-heading">Messereignis simulieren</h2>
		<div class="row">
			<label>Bahn
				<input type="number" min="1" max="4" bind:value={sensorLane} />
			</label>
			<label>Flanke
				<select bind:value={sensorEdge}>
					<option value="rising">steigend</option>
					<option value="falling">fallend</option>
				</select>
			</label>
			<button type="button" onclick={sensor}>Messereignis senden</button>
		</div>
	</section>

	<section aria-labelledby="correction-heading">
		<h2 id="correction-heading">Rundenkorrektur</h2>
		<div class="row">
			<label>Bahn
				<input type="number" min="1" max="4" bind:value={correctionLane} />
			</label>
			<label>Runden (auch Bruchteile, z.&nbsp;B. 0,5)
				<input type="text" inputmode="decimal" bind:value={correctionLaps} />
			</label>
			<button type="button" onclick={correct}>Korrektur anwenden</button>
		</div>
	</section>

	<section aria-labelledby="state-heading">
		<h2 id="state-heading">Rennstand</h2>
		<LaneTable lanes={snapshot?.state?.lanes ?? []} />
	</section>
</main>

<style>
	:global(body) {
		margin: 0;
		background: #fff;
		color: #111;
		font-family: system-ui, sans-serif;
	}
	main {
		max-width: 70rem;
		margin: 0 auto;
		padding: 1rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}
	h1,
	h2 {
		margin: 0 0 0.5rem;
	}
	p {
		margin: 0;
		display: flex;
		gap: 1.5rem;
		flex-wrap: wrap;
	}
	.error {
		color: #a40000;
		font-weight: bold;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}
	.row {
		display: flex;
		flex-wrap: wrap;
		align-items: end;
		gap: 0.75rem;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}
	input,
	select,
	button {
		font: inherit;
		padding: 0.4em 0.6em;
	}
	button {
		cursor: pointer;
	}
</style>
