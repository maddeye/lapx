<script>
	import { onMount } from 'svelte';
	import {
		createStateClient,
		connectState,
		displayRaceElapsed,
		displayProtocolNow,
		postCommand,
		formatMs,
		phaseText
	} from '$lib/state.js';
	import {
		startPayload,
		sensorPayload,
		raceChaosPayload,
		laneChaosPayload,
		correctionPayload
	} from '$lib/control.js';
	import LaneTable from '$lib/LaneTable.svelte';

	let snapshot = $state(null);
	let connection = $state('verbinde …');
	let connected = $state(false);
	let error = $state('');
	let receivedAt = $state(0);
	let disconnectedAt = $state(null);
	let clock = $state(0);
	let pending = $state(false);
	let drivers = $state([]);

	let lanes = $state(2);
	let driverIds = $state(['', '', '', '']);
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

	let accept = () => {};

	onMount(() => {
		clock = performance.now();
		const client = createStateClient((next) => {
			snapshot = next.snapshot;
			connection = next.connection;
			connected = next.connected;
			receivedAt = next.receivedAt;
			disconnectedAt = next.disconnectedAt;
		});
		const stream = connectState(client);
		accept = stream.accept;
		fetch('/api/drivers')
			.then((response) => response.ok ? response.json() : Promise.reject(new Error(`Fehler ${response.status}`)))
			.then((items) => (drivers = items.filter((driver) => driver.archived_at === null)))
			.catch((failure) => (error = failure.message));
		const timer = setInterval(() => (clock = performance.now()), 100);
		return () => {
			clearInterval(timer);
			stream.stop();
		};
	});

	const raceElapsed = $derived(
		displayRaceElapsed(snapshot, receivedAt, connected, clock, disconnectedAt)
	);
	const protocolNow = $derived(
		displayProtocolNow(snapshot, receivedAt, connected, clock, disconnectedAt)
	);

	async function run(action) {
		if (pending) return;
		pending = true;
		error = '';
		try {
			accept(await action());
		} catch (failure) {
			error = failure.message;
		} finally {
			pending = false;
		}
	}

	const start = () => run(() => postCommand('/api/start', startPayload({
		lanes,
		driverIds,
		startSequenceMs,
		restartSequenceMs,
		minimumLapTimeMs,
		finishKind,
		finishLaps,
		finishTimeMs,
		finishMode,
		falseStartKind,
		falseStartMs,
		chaosKind,
		chaosMs
	})));
	const pause = () => run(() => postCommand('/api/pause'));
	const resume = () => run(() => postCommand('/api/resume'));
	const raceChaos = () => run(() => postCommand('/api/chaos', raceChaosPayload()));
	const laneChaos = () => run(() => postCommand('/api/chaos', laneChaosPayload(chaosLane)));
	const sensor = () => run(() => postCommand('/api/sensor', sensorPayload(sensorLane, sensorEdge)));
	const correct = () =>
		run(() => postCommand('/api/correct-laps', correctionPayload(correctionLane, correctionLaps)));
</script>

<svelte:head>
	<title>LapX Rennleitung</title>
</svelte:head>

<main>
	<h1>Rennleitung</h1>
	<p aria-live="polite">
		<span>Phase: {phaseText(snapshot)}</span>
		<span>Rennzeit: {formatMs(raceElapsed)}</span>
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
				{#each Array.from({ length: lanes }, (_, index) => index + 1) as lane}
					<label>Fahrer Bahn {lane}
						<select bind:value={driverIds[lane - 1]}>
							<option value="">Anonym</option>
							{#each drivers as driver (driver.id)}
								<option value={driver.id}>{driver.display_name}</option>
							{/each}
						</select>
					</label>
				{/each}
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
			<button type="submit" disabled={pending}>Rennen starten</button>
		</form>
	</section>

	<section aria-labelledby="control-heading">
		<h2 id="control-heading">Rennsteuerung</h2>
		<div class="row">
			<button type="button" disabled={pending} onclick={pause}>Rennpause</button>
			<button type="button" disabled={pending} onclick={resume}>Wiederanlauf</button>
			<button type="button" disabled={pending} onclick={raceChaos}>Chaos Rennleitung</button>
		</div>
		<div class="row">
			<label>Chaos-Bahn
				<input type="number" min="1" max="4" bind:value={chaosLane} />
			</label>
			<button type="button" disabled={pending} onclick={laneChaos}>Chaos Bahn auslösen</button>
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
			<button type="button" disabled={pending} onclick={sensor}>Messereignis senden</button>
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
			<button type="button" disabled={pending} onclick={correct}>Korrektur anwenden</button>
		</div>
	</section>

	<section aria-labelledby="state-heading">
		<h2 id="state-heading">Rennstand</h2>
		<LaneTable lanes={snapshot?.state?.lanes ?? []} {snapshot} {protocolNow} />
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
