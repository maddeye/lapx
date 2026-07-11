<script>
	import { onMount } from 'svelte';
	import { connectState, formatMs, phaseText } from '$lib/state.js';
	import LaneTable from '$lib/LaneTable.svelte';

	let snapshot = $state(null);
	let connection = $state('verbinde …');

	onMount(() =>
		connectState(
			(next) => (snapshot = next),
			(status) => (connection = status)
		)
	);

	const lanes = $derived(snapshot?.state?.lanes ?? []);
</script>

<svelte:head>
	<title>LapX Rennscreen</title>
</svelte:head>

<main>
	<header>
		<h1>Rennscreen</h1>
		<p aria-live="polite">
			<span>Phase: {phaseText(snapshot)}</span>
			<span>Verbindung: {connection}</span>
		</p>
	</header>
	<p class="race-time" aria-label="Rennzeit">{formatMs(snapshot?.race_elapsed_ms)}</p>
	<LaneTable {lanes} />
</main>

<style>
	:global(body) {
		margin: 0;
		background: #111;
		color: #f2f2f2;
		font-family: system-ui, sans-serif;
	}
	main {
		min-height: 100vh;
		padding: 2vh 3vw;
		display: flex;
		flex-direction: column;
		gap: 1.5vh;
	}
	header {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		justify-content: space-between;
		gap: 1rem;
	}
	h1 {
		margin: 0;
		font-size: clamp(1.2rem, 3vw, 2rem);
	}
	header p {
		margin: 0;
		display: flex;
		gap: 1.5rem;
		font-size: clamp(1rem, 2vw, 1.4rem);
	}
	.race-time {
		margin: 0;
		text-align: center;
		font-size: clamp(3rem, 14vw, 10rem);
		font-variant-numeric: tabular-nums;
		line-height: 1.1;
	}
</style>
