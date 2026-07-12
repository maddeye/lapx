<script>
	import { formatMs, formatLaps, currentLapMs } from '$lib/state.js';

	let { lanes = [], snapshot = null, protocolNow = null } = $props();
</script>

<table>
	<caption class="visually-hidden">Rundenstand je Bahn</caption>
	<thead>
		<tr>
			<th scope="col">Bahn</th>
			<th scope="col">Runden</th>
			<th scope="col">Korrigierte Runden</th>
			<th scope="col">Letzte Runde</th>
			<th scope="col">Aktuelle Runde</th>
			<th scope="col">Beste Runde</th>
		</tr>
	</thead>
	<tbody>
		{#each lanes as lane (lane.lane)}
			<tr>
				<th scope="row">Bahn {lane.lane}</th>
				<td class="numeric">{lane.laps}</td>
				<td class="numeric">{formatLaps(lane.corrected_laps_thousandths)}</td>
				<td class="numeric">{formatMs(lane.last_lap_ms)}</td>
				<td class="numeric">{formatMs(currentLapMs(snapshot, lane, protocolNow))}</td>
				<td class="numeric">{formatMs(lane.best_lap_ms)}</td>
			</tr>
		{/each}
	</tbody>
</table>

<style>
	table {
		width: 100%;
		border-collapse: collapse;
		font-variant-numeric: tabular-nums;
	}
	th,
	td {
		padding: 0.4em 0.6em;
		text-align: left;
		border-bottom: 1px solid #444;
		font-size: clamp(1rem, 3.5vw, 2.5rem);
	}
	.numeric {
		text-align: right;
	}
	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
	}
</style>
