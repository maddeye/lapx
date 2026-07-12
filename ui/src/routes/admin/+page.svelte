<script>
	import { onMount } from 'svelte';
	import { formatLaps, formatMs } from '$lib/state.js';

	let { initialDrivers = [] } = $props();
	// svelte-ignore state_referenced_locally
	let drivers = $state(initialDrivers);
	let history = $state([]);
	let stats = $state([]);
	let displayName = $state('');
	let pending = $state(false);
	let error = $state('');

	async function request(path, body) {
		const response = await fetch(path, {
			method: body === undefined ? 'GET' : 'POST',
			headers: body === undefined ? {} : { 'content-type': 'application/json' },
			body: body === undefined ? undefined : JSON.stringify(body)
		});
		if (!response.ok) throw new Error((await response.text()) || `Fehler ${response.status}`);
		return response.json();
	}

	async function run(action) {
		if (pending) return;
		pending = true;
		error = '';
		try {
			await action();
		} catch (failure) {
			error = failure.message;
		} finally {
			pending = false;
		}
	}

	const load = () => run(async () => {
		[drivers, history, stats] = await Promise.all([
			request('/api/drivers'),
			request('/api/race-history'),
			request('/api/driver-stats')
		]);
	});
	const driverName = (id) => drivers.find((driver) => driver.id === id)?.display_name ?? `Fahrer ${id}`;
	const create = () => run(async () => {
		const driver = await request('/api/drivers', { display_name: displayName });
		drivers = [...drivers, driver];
		displayName = '';
	});
	const rename = (id, event) => run(async () => {
		const display_name = new FormData(event.currentTarget).get('display_name');
		const renamed = await request(`/api/drivers/${id}/rename`, { display_name });
		drivers = drivers.map((driver) => driver.id === id ? renamed : driver);
	});
	const archive = (id) => run(async () => {
		const archived = await request(`/api/drivers/${id}/archive`, {});
		drivers = drivers.map((driver) => driver.id === id ? archived : driver);
	});

	onMount(load);
</script>

<svelte:head>
	<title>LapX Fahrer</title>
</svelte:head>

<main>
	<h1>Fahrer</h1>
	{#if error}
		<p class="error" role="alert">Fehler: {error}</p>
	{/if}

	<section aria-labelledby="create-heading">
		<h2 id="create-heading">Fahrer anlegen</h2>
		<form onsubmit={(event) => (event.preventDefault(), create())}>
			<label>Anzeigename
				<input required bind:value={displayName} />
			</label>
			<button type="submit" disabled={pending}>Anlegen</button>
		</form>
	</section>

	<section aria-labelledby="list-heading">
		<h2 id="list-heading">Fahrerliste</h2>
		{#if drivers.length === 0}
			<p>Keine Fahrer vorhanden.</p>
		{:else}
			<ul>
				{#each drivers as driver (driver.id)}
					<li>
						<form onsubmit={(event) => (event.preventDefault(), rename(driver.id, event))}>
							<label>Name
								<input name="display_name" required value={driver.display_name} />
							</label>
							<button type="submit" disabled={pending}>Umbenennen</button>
							<button type="button" disabled={pending || driver.archived_at !== null} onclick={() => archive(driver.id)}>Archivieren</button>
							{#if driver.archived_at !== null}<span>Archiviert</span>{/if}
						</form>
					</li>
				{/each}
			</ul>
		{/if}
	</section>

	<section aria-labelledby="stats-heading">
		<h2 id="stats-heading">Fahrerstatistik</h2>
		{#if stats.length === 0}
			<p>Keine abgeschlossenen Rennen vorhanden.</p>
		{:else}
			<table>
				<thead><tr><th>Fahrer</th><th>Starts</th><th>Siege</th><th>Beste gültige Runde</th></tr></thead>
				<tbody>
					{#each stats as stat (stat.driver_id)}
						<tr><th>{driverName(stat.driver_id)}</th><td>{stat.starts}</td><td>{stat.wins}</td><td>{formatMs(stat.best_lap_ms)}</td></tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>

	<section aria-labelledby="history-heading">
		<h2 id="history-heading">Rennhistorie</h2>
		{#if history.length === 0}
			<p>Keine abgeschlossenen Rennen vorhanden.</p>
		{:else}
			{#each history as race (race.race_id)}
				<h3>{race.race_id}</h3>
				<table>
					<thead><tr><th>Platz</th><th>Bahn</th><th>Fahrer</th><th>Runden</th><th>Ergebniszeit</th><th>Beste Runde</th></tr></thead>
					<tbody>
						{#each race.results as result (result.lane)}
							<tr>
								<td>{result.position}</td><td>{result.lane}</td>
								<td>{result.driver_id === null ? 'Anonym' : driverName(result.driver_id)}</td>
								<td>{formatLaps(result.corrected_laps_thousandths)}</td>
								<td>{formatMs(result.result_time_ms)}</td><td>{formatMs(result.best_lap_ms)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/each}
		{/if}
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
		max-width: 55rem;
		margin: 0 auto;
		padding: 1rem;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}
	h1, h2, h3, p, ul { margin: 0; }
	.error { color: #a40000; font-weight: bold; }
	ul { padding: 0; list-style: none; }
	li { padding: 0.75rem 0; border-top: 1px solid #bbb; }
	form { display: flex; flex-wrap: wrap; align-items: end; gap: 0.75rem; }
	label { display: flex; flex-direction: column; gap: 0.25rem; }
	input, button { font: inherit; padding: 0.4em 0.6em; }
	button { cursor: pointer; }
	table { width: 100%; border-collapse: collapse; }
	th, td { padding: 0.4em; text-align: left; border-bottom: 1px solid #bbb; }
</style>
