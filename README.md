# LapX

LapX is a local race-control and lap-timing server for Carrera slot-car races. It stores every race-relevant event in a SQLite **Rennprotokoll**, serves a read-only Rennscreen, and provides local operator, debug, and administration pages.

## Quick desktop test

### Requirements

- Rust toolchain (`cargo`)
- A modern browser
- Node.js/npm only if you want to rebuild or test the Svelte UI

### 1. Start a clean simulated server

From the repository root:

```bash
rm -f /tmp/lapx-test.db /tmp/lapx-test.db-wal /tmp/lapx-test.db-shm
LAPX_DB=/tmp/lapx-test.db \
LAPX_LOCAL_BIND=127.0.0.1:3000 \
LAPX_PUBLIC_BIND=127.0.0.1:3001 \
cargo run --bin lapxd
```

Leave this terminal running. No `LAPX_HARDWARE` means that LapX runs without GPIO and exposes controls for simulated Messereignisse.

### 2. Open the pages

| URL | Purpose |
|---|---|
| <http://localhost:3000/control> | Local Rennleiter controls |
| <http://localhost:3000/> | Rennscreen on the local bind |
| <http://localhost:3001/> | Public read-only Rennscreen |
| <http://localhost:3000/debug> | Lightweight Debug-Bedienung |
| <http://localhost:3000/admin> | Fahrer, history, Elo, and tournaments |
| <http://localhost:3000/hardware> | Hardware diagnostics page; its API reports unavailable without GPIO configuration |

The local server deliberately accepts only loopback `Host` values. Use `localhost` or `127.0.0.1`, not a custom hostname.

### 3. Run a short simulated race

On `/control`:

1. Set **Bahnen** to `1`.
2. Set **Startsequenz** and **Wiederanlaufsequenz** to `500` ms.
3. Set **Mindestrundenzeit** to `100` ms.
4. Select **Runden**, `3` Zielrunden, and **Sofort**.
5. Click **Rennen starten** and wait for the start sequence to finish.
6. Under **Messereignis simulieren**, send three rising events for Bahn 1, at least 100 ms apart.
7. Confirm that the race finishes after the third valid lap and updates both Rennscreens.
8. Try **Rennpause** and **Wiederanlauf** during another race.
9. Apply a post-race **Rundenkorrektur** and inspect `/admin` to see history update. Fahrer statistics also update when the race used a named Fahrer.

After a finished or aborted race, enter a fresh **Nächste Renn-ID** on `/control` to start another race. The selected race and its state survive a server restart.

## Test the HTTP API directly

With `lapxd` running, start a one-lane race:

```bash
curl -sS http://localhost:3000/api/start \
  -H 'content-type: application/json' \
  -d '{
    "config": {
      "lanes": 1,
      "driver_ids": [null],
      "start_sequence_ms": 500,
      "restart_sequence_ms": 500,
      "minimum_lap_time_ms": 100,
      "finish_condition": {"laps": 3},
      "finish_mode": "immediate",
      "false_start_consequence": "abort",
      "chaos_consequence": "abort"
    }
  }'
```

Wait at least 600 ms (500 ms start sequence plus the 100 ms Mindestrundenzeit), then simulate laps:

```bash
sleep 0.6

for lap in 1 2 3; do
  curl -sS http://localhost:3000/api/sensor \
    -H 'content-type: application/json' \
    -d '{"lane":1,"edge":"rising"}' >/dev/null
  sleep 0.15
done

curl -sS http://localhost:3000/api/state
```

Pause and resume mutations require an empty JSON object:

```bash
curl -sS http://localhost:3000/api/pause \
  -H 'content-type: application/json' -d '{}'

curl -sS http://localhost:3000/api/resume \
  -H 'content-type: application/json' -d '{}'
```

The public port provides only `/`, `/api/state`, `/api/state/stream`, and Rennscreen assets. Mutating and local diagnostic routes return `404` there.

## Test with `lapxctl`

`lapxctl` writes through the same SQLite event-store path. Unlike `lapxd`, it has no due-event worker, so the example explicitly advances the start sequence.

```bash
export LAPX_DB=/tmp/lapxctl-test.db
rm -f "$LAPX_DB" "$LAPX_DB-wal" "$LAPX_DB-shm"

cargo run --quiet --bin lapxctl -- start --json - <<'JSON'
{
  "race_id": "cli-race",
  "at": 0,
  "config": {
    "lanes": 1,
    "driver_ids": [null],
    "start_sequence_ms": 500,
    "restart_sequence_ms": 500,
    "minimum_lap_time_ms": 100,
    "finish_condition": {"laps": 3},
    "finish_mode": "immediate",
    "false_start_consequence": "abort",
    "chaos_consequence": "abort"
  }
}
JSON

printf '%s\n' '{"race_id":"cli-race","to":500}' \
  | cargo run --quiet --bin lapxctl -- advance --json -

for at in 600 800 1000; do
  printf '%s\n' "{\"race_id\":\"cli-race\",\"lane\":1,\"at\":$at,\"edge\":\"rising\"}" \
    | cargo run --quiet --bin lapxctl -- sensor --json -
done
```

Supported commands are `start`, `advance`, `sensor`, `correct`, `pause`, `resume`, and `chaos`. Run a command with the wrong arguments to print its compact usage line.

Do not write to the same active race simultaneously with both `lapxctl` and the UI unless you intentionally want to test external concurrent commits.

## Fahrer, history, Elo, and tournaments

Open <http://localhost:3000/admin> to:

- create, rename, and archive Fahrer;
- view corrected race history and Fahrer statistics;
- inspect derived Elo ratings;
- create manual tournament heat lists and link them to race IDs;
- generate deterministic random or Elo-balanced tournaments from a Fahrer set and seed.

Create Fahrer before starting a race if you want named history. Archived Fahrer remain visible in historical results but cannot be assigned to new races.

## Persistence and reset

`LAPX_DB` selects the SQLite database; the default is `./lapx.db`. Restarting `lapxd` rebuilds race state from its Rennprotokoll. An active hardware race is recovered into Rennpause and requires Wiederanlauf before lane power returns.

To reset a test installation, stop `lapxd` first and remove all three SQLite files:

```bash
rm -f lapx.db lapx.db-wal lapx.db-shm
```

## Raspberry Pi GPIO mode

> Verify every BCM pin, input pull, edge, relay polarity, and all-off output with the actual wiring and a meter before connecting track power.

Build and run with the `gpio` feature and `LAPX_HARDWARE`:

```bash
LAPX_DB=/var/lib/lapx/lapx.db \
LAPX_LOCAL_BIND=127.0.0.1:3000 \
LAPX_PUBLIC_BIND=0.0.0.0:3001 \
LAPX_HARDWARE='1:17:rising:up:22:active_low,2:27:rising:up:23:active_low,3:5:rising:up:24:active_low,4:6:rising:up:25:active_low' \
cargo run --release --features gpio --bin lapxd
```

The example pin numbers are illustrative, not a wiring recommendation. Each comma-separated mapping is:

```text
lane:input_bcm_pin:edge:pull:relay_bcm_pin:polarity
```

Accepted values:

- `edge`: `rising` or `falling`
- `pull`: `off`, `up`, or `down`
- `polarity`: `active_high`/`high` or `active_low`/`low`

All input and relay BCM pins must be unique, and lanes must be contiguous from 1. On startup, LapX commands all configured relays off before recovering race state.

Because the control bind must remain loopback-only, operate a remote Pi through an SSH tunnel:

```bash
ssh -L 3000:127.0.0.1:3000 pi@YOUR_PI
```

Then open <http://localhost:3000/control> on your computer. The public Rennscreen remains available on the Pi's port 3001 if allowed by its firewall.

## Run the automated checks

```bash
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

cd ui
npm ci
npm test
```

`npm test` runs UI unit/render checks, Svelte diagnostics, and verifies that the committed embedded build matches a fresh build.
