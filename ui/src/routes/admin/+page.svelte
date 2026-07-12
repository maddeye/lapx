<script>
	import { onMount } from 'svelte';
	import { formatLaps, formatMs } from '$lib/state.js';

	let {
		initialDrivers = [],
		initialElo = { ratings: [], races: [] },
		initialTournaments = [],
		initialSelectedTournament = null
	} = $props();
	// svelte-ignore state_referenced_locally
	let drivers = $state(initialDrivers);
	let history = $state([]);
	let stats = $state([]);
	// svelte-ignore state_referenced_locally
	let elo = $state(initialElo);
	// svelte-ignore state_referenced_locally
	let tournaments = $state(initialTournaments);
	// svelte-ignore state_referenced_locally
	let selectedTournament = $state(initialSelectedTournament);
	let displayName = $state('');
	let tournamentName = $state('');
	let generatedName = $state('');
	let generatedDriverIds = $state([]);
	let generatedLanes = $state(2);
	let generatedMode = $state('random');
	let generatedSeed = $state('0');
	let heatLanes = $state(2);
	let heatDriverIds = $state(['', '', '', '']);
	let pending = $state(false);
	let error = $state('');

	async function request(path, body) {
		const response = await fetch(path, {
			method: body === undefined ? 'GET' : 'POST',
			headers: body === undefined ? {} : { 'content-type': 'application/json' },
			body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body)
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
		[drivers, history, stats, elo, tournaments] = await Promise.all([
			request('/api/drivers'),
			request('/api/race-history'),
			request('/api/driver-stats'),
			request('/api/elo'),
			request('/api/tournaments')
		]);
	});
	const driverName = (id) => drivers.find((driver) => driver.id === id)?.display_name ?? `Fahrer ${id}`;
	const eloRating = (id) => elo.ratings.find((rating) => rating.driver_id === id)?.rating ?? 1500;
	const activeDrivers = $derived(drivers.filter((driver) => driver.archived_at === null));
	function selectTournament(tournament) {
		selectedTournament = tournament;
		tournaments = tournaments.map((item) => item.id === tournament.id ? tournament : item);
	}
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
	const createTournament = () => run(async () => {
		const tournament = await request('/api/tournaments', { name: tournamentName });
		tournaments = [...tournaments, tournament];
		selectedTournament = tournament;
		tournamentName = '';
	});
	const generateTournament = () => run(async () => {
		if (!/^\d+$/.test(generatedSeed) || BigInt(generatedSeed) > 18446744073709551615n) {
			throw new Error('Seed muss eine Zahl von 0 bis 18446744073709551615 sein');
		}
		const body = `{"name":${JSON.stringify(generatedName)},"driver_ids":${JSON.stringify(generatedDriverIds)},"lane_count":${generatedLanes},"mode":${JSON.stringify(generatedMode)},"seed":${generatedSeed}}`;
		const tournament = await request('/api/tournaments/generate', body);
		tournaments = [...tournaments, tournament];
		selectedTournament = tournament;
		generatedName = '';
		generatedDriverIds = [];
	});
	const openTournament = (id) => run(async () => selectTournament(await request(`/api/tournaments/${id}`)));
	const appendHeat = () => run(async () => {
		const assignments = heatDriverIds.slice(0, heatLanes).map((driverId, index) => ({
			lane: index + 1,
			driver_id: Number(driverId)
		}));
		selectTournament(await request(`/api/tournaments/${selectedTournament.id}/heats`, { assignments }));
		heatDriverIds = ['', '', '', ''];
	});
	const linkHeat = (heatId, event) => run(async () => {
		const race_id = new FormData(event.currentTarget).get('race_id');
		selectTournament(await request(`/api/tournaments/${selectedTournament.id}/heats/${heatId}/link`, { race_id }));
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
			<table>
				<thead><tr><th>Fahrer</th><th>Elo</th></tr></thead>
				<tbody>
					{#each drivers as driver (driver.id)}
						<tr>
							<td>
								<form onsubmit={(event) => (event.preventDefault(), rename(driver.id, event))}>
									<label>Name
										<input name="display_name" required value={driver.display_name} />
									</label>
									<button type="submit" disabled={pending}>Umbenennen</button>
									<button type="button" disabled={pending || driver.archived_at !== null} onclick={() => archive(driver.id)}>Archivieren</button>
									{#if driver.archived_at !== null}<span>Archiviert</span>{/if}
								</form>
							</td>
							<td>{eloRating(driver.id)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>

	<section aria-labelledby="tournaments-heading">
		<h2 id="tournaments-heading">Turniere</h2>
		<form onsubmit={(event) => (event.preventDefault(), createTournament())}>
			<label>Turniername
				<input required bind:value={tournamentName} />
			</label>
			<button type="submit" disabled={pending}>Turnier anlegen</button>
		</form>
		<form onsubmit={(event) => (event.preventDefault(), generateTournament())}>
			<label>Name des generierten Turniers
				<input required bind:value={generatedName} />
			</label>
			<fieldset>
				<legend>Fahrer auswählen</legend>
				{#each activeDrivers as driver (driver.id)}
					<label class="checkbox"><input type="checkbox" value={driver.id} bind:group={generatedDriverIds} /> {driver.display_name}</label>
				{/each}
			</fieldset>
			<label>Generierungsmodus
				<select bind:value={generatedMode}>
					<option value="random">Zufällig</option>
					<option value="elo_balanced">Elo-ausgeglichen</option>
				</select>
			</label>
			<label>Seed (0 bis 18446744073709551615)
				<input required inputmode="numeric" pattern="[0-9]+" bind:value={generatedSeed} />
			</label>
			<label>Bahnen pro Lauf
				<select bind:value={generatedLanes}>
					{#each [1, 2, 3, 4] as count}<option value={count}>{count}</option>{/each}
				</select>
			</label>
			<button type="submit" disabled={pending || generatedDriverIds.length < 2}>Turnier generieren</button>
		</form>
		{#if tournaments.length === 0}
			<p>Keine Turniere vorhanden.</p>
		{:else}
			<ul>
				{#each tournaments as tournament (tournament.id)}
					<li>
						<strong>{tournament.name}</strong>
						<button type="button" disabled={pending} onclick={() => openTournament(tournament.id)}>Anzeigen</button>
					</li>
				{/each}
			</ul>
		{/if}

		{#if selectedTournament}
			<h3>{selectedTournament.name}: Läufe</h3>
			{#if selectedTournament.generation}
				<p>Generiert: {selectedTournament.generation.mode === 'random' ? 'Zufällig' : 'Elo-ausgeglichen'} · Seed {selectedTournament.generation.seed} · {selectedTournament.generation.lane_count} Bahnen</p>
			{/if}
			<form onsubmit={(event) => (event.preventDefault(), appendHeat())}>
				<label>Bahnen
					<select bind:value={heatLanes}>
						{#each [1, 2, 3, 4] as count}<option value={count}>{count}</option>{/each}
					</select>
				</label>
				{#each Array(heatLanes) as _, index}
					<label>Fahrer Bahn {index + 1}
						<select required bind:value={heatDriverIds[index]}>
							<option value="">Bitte wählen</option>
							{#each activeDrivers as driver (driver.id)}
								<option value={driver.id}>{driver.display_name}</option>
							{/each}
						</select>
					</label>
				{/each}
				<button type="submit" disabled={pending}>Lauf anhängen</button>
			</form>

			{#if selectedTournament.heats.length === 0}
				<p>Keine Läufe vorhanden.</p>
			{:else}
				<ol>
					{#each selectedTournament.heats as heat (heat.id)}
						<li>
							<h4>Lauf {heat.position}</h4>
							<p>{heat.assignments.map((assignment) => `Bahn ${assignment.lane}: ${driverName(assignment.driver_id)}`).join(' · ')}</p>
							{#if heat.race_id === null}
								<form onsubmit={(event) => (event.preventDefault(), linkHeat(heat.id, event))}>
									<label>Renn-ID
										<input name="race_id" required />
									</label>
									<button type="submit" disabled={pending}>Rennen einmalig verknüpfen</button>
								</form>
							{:else}
								<p>Rennen: {heat.race_id}</p>
								<table>
									<thead><tr><th>Bahn</th><th>Fahrer</th><th>Aktuelle Runden</th><th>Ergebniszeit</th></tr></thead>
									<tbody>
										{#each heat.results ?? [] as result (result.lane)}
											<tr><td>{result.lane}</td><td>{driverName(result.driver_id)}</td><td>{formatLaps(result.corrected_laps_thousandths)}</td><td>{formatMs(result.result_time_ms)}</td></tr>
										{/each}
									</tbody>
								</table>
							{/if}
						</li>
					{/each}
				</ol>
			{/if}
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
	fieldset { display: flex; flex-wrap: wrap; gap: 0.5rem; }
	.checkbox { flex-direction: row; align-items: center; }
	input, select, button { font: inherit; padding: 0.4em 0.6em; }
	button { cursor: pointer; }
	table { width: 100%; border-collapse: collapse; }
	th, td { padding: 0.4em; text-align: left; border-bottom: 1px solid #bbb; }
</style>
