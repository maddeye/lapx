# LapX implementation TODO

Complete these in order. Keep each change limited to its linked slice; do not pull later non-goals forward.

## Completion rules

- [ ] Implement every delivery item below in order.
- [ ] Keep `RaceEngine` independent of UI, transport, storage, and hardware.
- [ ] Commit race-relevant events to the SQLite **Rennprotokoll** before exposing their state.
- [ ] Add only the smallest runnable check named by each slice.
- [ ] Run all existing checks after every delivery item.
- [ ] Keep the linked HTML plan current if implementation discoveries change the path.
- [ ] Finish with all Rust, UI, integration, and Raspberry Pi hardware checks passing.

## Foundation

- [x] **Delivery 1 — [Race configuration shell](.plans/best-path/slice-01.html)**
  - Define `RaceConfig`, commands, events, state, errors, and validation for 1–4 lanes.
  - Verify: `cargo test invalid_start_config invalid_control_commands`.

- [x] **Delivery 2 — [Start race and replay projection](.plans/best-path/slice-02.html)**
  - Start a race through events and rebuild identical state by replay.
  - Verify: `cargo test replay`.

- [x] **Delivery 3 — [Official start as a due event](.plans/best-path/slice-03.html)**
  - Add protocol milliseconds, `AdvanceRace`, ordered due events, and timestamp rejection.
  - Verify: `cargo test start_sequence replay_at commands_before_last`.

- [x] **Delivery 4 — [Messereignis to gültige Runde](.plans/best-path/slice-04.html)**
  - Record measurements and enforce `Mindestrundenzeit` from official start and the previous accepted lap.
  - Verify: `cargo test mindestrundenzeit`.

## First complete and durable race

- [x] **Delivery 5 — [Zielbedingung, Zielmodus, and Rundenkorrektur](.plans/best-path/slice-08.html)**
  - Finish lap- and time-limited races, apply all finish modes, and support event-based corrections.
  - Verify: `cargo test finish live_correction sensor_after_due_time`.

- [x] **Delivery 6 — [SQLite Rennprotokoll and lapxctl](.plans/best-path/slice-09.html)**
  - Add versioned append-only events, `BEGIN IMMEDIATE`, durable replay, and scriptable JSON CLI commands.
  - Verify: `cargo test store_cli sqlite_store_replays_committed_events` plus concurrent-writer coverage.

## Race completeness

- [x] **Delivery 7 — [Fehlstartfolge](.plans/best-path/slice-05.html)**
  - Detect Fehlstart and apply abort, result-time penalty, or intended lane power-off.
  - Extend `lapxctl` through the existing command/store path.
  - Verify: `cargo test false_start messereignis_during_startsequence`.

- [x] **Delivery 8 — [Rennpause and Wiederanlaufsequenz](.plans/best-path/slice-06.html)**
  - Freeze race time and intended power, continue accepting rolling laps, and resume through a due sequence.
  - Extend `lapxctl` with pause and resume.
  - Verify: `cargo test rennpause restart_sequence resume_sequence`.

- [x] **Delivery 9 — [Chaosauslösung](.plans/best-path/slice-07.html)**
  - Reuse Rennpause and Fehlstartfolge behavior for lane and race-control Chaostaster commands.
  - Extend `lapxctl` with chaos commands.
  - Verify: `cargo test chaos lane_power_off_expiry`.

## Local integration and Debug-Bedienung

- [x] **Delivery 10 — [Local HTTP state and due-event runtime](.plans/best-path/slice-10.html)**
  - Serve `GET /api/state` on loopback and materialize due events without client activity.
  - Verify: `cargo test http_state_returns_json runtime_materializes_due_event`.

- [x] **Delivery 11 — [Local HTTP commands](.plans/best-path/slice-11.html)**
  - Map explicit POST routes to the existing durable command path without duplicating rules.
  - Verify: `cargo test http_command_round_trip`.

- [x] **Delivery 12 — [SSE state stream](.plans/best-path/slice-12.html)**
  - Add `RaceRuntime`, post-commit full-state broadcasts, sequence filtering, and reconnect-safe subscription.
  - Verify: `cargo test sse_emits_state_after_command` plus connection-race coverage.

- [x] **Delivery 13 — [Minimal Debug-Bedienung](.plans/best-path/slice-13.html)**
  - Serve one static local debug page using `fetch` and `EventSource`.
  - Verify: `cargo test debug_page_loads`; manually start a race and accept one lap without refresh.

- [x] **Delivery 14 — [Debug configuration and corrections](.plans/best-path/slice-14.html)**
  - Add race configuration and post-race Rundenkorrektur controls to the same debug page.
  - Verify: `cargo test http_correction_updates_finished_race`.

## Raspberry Pi hardware

- [x] **Delivery 15 — [TimingSource and PowerOutput seams](.plans/best-path/slice-15.html)**
  - Route simulated timing, HTTP, and due events through `RaceRuntime`; synchronize power only after commits.
  - Verify: `cargo test simulation_timing_source_triggers_lap`.

- [ ] **Delivery 16 — [GPIO Messereignisse](.plans/best-path/slice-16.html)**
  - Capture configured Raspberry Pi input edges with `rppal`, timestamp first, then enqueue commands.
  - Verify: `cargo test --features gpio` and Pi checks for lane mapping and ≤1 ms timestamp resolution.
  - Software/feature checks pass; physical Raspberry Pi timing and lane mapping remain pending.

- [ ] **Delivery 17 — [Relay lane power](.plans/best-path/slice-17.html)**
  - Add configured relay outputs, fail-safe startup off, and operator-required resume after recovery.
  - Verify: `cargo test relay_power_follows_race_state startup_all_lanes_off recovered_race_requires_resume` and meter checks on the Pi.
  - Software fail-safe checks pass; physical relay polarity and all-off meter checks remain pending.

- [ ] **Delivery 18 — [Hardware debug page](.plans/best-path/slice-18.html)**
  - Add a local-only snapshot of configured pins, levels, outputs, and latest raw edges.
  - Verify: `cargo test hardware_page_loads_snapshot` and confirm Pi lane/pin mapping.
  - Snapshot/page checks pass; confirmation against live Pi pins remains pending.

## Safe production UI

- [x] **Delivery 19 — [Public read-only binding](.plans/best-path/slice-19.html)**
  - Separate local mutating routes from public state, SSE, and display routes.
  - Verify: `cargo test public_api_is_read_only`.

- [x] **Delivery 20 — [Rennscreen](.plans/best-path/slice-20.html)**
  - Build one static SvelteKit fullscreen display and serve its assets from Rust.
  - Verify: `npm test` and `cargo test serves_static_rennscreen`.

- [x] **Delivery 21 — [Local Rennleiter UI](.plans/best-path/slice-21.html)**
  - Add local production controls over existing commands; keep `/control` unavailable publicly.
  - Verify: UI smoke test, complete-race walkthrough, and public `/control` rejection.

## Optional administration

- [x] **Delivery 22 — [Fahrer CRUD](.plans/best-path/slice-22.html)**
  - Add stable Fahrer identities with list, create, rename, and archive operations.
  - Verify: `cargo test driver_crud_round_trip`.

- [x] **Delivery 23 — [Race history statistics](.plans/best-path/slice-23.html)**
  - Assign Fahrer through race events and derive corrected history, starts, wins, and best laps from Rennprotokolle.
  - Verify: `cargo test driver_stats_from_completed_race correction_updates_driver_stats`.

- [x] **Delivery 24 — [Manual tournament heat list](.plans/best-path/slice-25.html)**
  - Add tournaments, ordered heats, lane assignments, and links to authoritative race ids.
  - Verify: `cargo test manual_tournament_flow`.

- [x] **Delivery 25 — [Elo](.plans/best-path/slice-24.html)**
  - Derive deterministic pairwise multi-lane Elo and rebuild later ratings after corrections.
  - Verify: `cargo test elo_is_reproducible correction_rebuilds_later_elo`.

- [x] **Delivery 26 — [Generated tournaments](.plans/best-path/slice-26.html)**
  - Add deterministic seeded random heat generation, then Elo-balanced snake distribution.
  - Verify: `cargo test tournament_generation_is_deterministic`.

## Final acceptance

- [x] A complete simulated race works through `lapxctl`, HTTP, Debug-Bedienung, and Rennleiter UI.
- [ ] A complete physical race works on Raspberry Pi with four Messstellen and four relay outputs.
- [x] Power loss/restart reconstructs committed state, leaves lanes unpowered, and requires Wiederanlaufsequenz.
- [x] Public clients can observe a live race but cannot invoke commands or load local control/debug pages.
- [x] Post-race Rundenkorrektur updates history, statistics, tournament results, and all later Elo values.
- [x] The same tournament mode, Fahrer set, and seed always generate identical heats.
- [ ] All repository tests and hardware checks pass with no unchecked delivery or acceptance item above.

Detailed overview: [.plans/best-path/index.html](.plans/best-path/index.html)
