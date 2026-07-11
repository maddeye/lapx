use lapx::{
    domain::*,
    store::{SqliteStore, StoreError},
};
use rusqlite::Connection;
use std::{
    io::Write,
    process::{Command as ProcessCommand, Stdio},
    sync::{Arc, Barrier},
    thread,
};
use tempfile::tempdir;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 10,
        restart_sequence_ms: 5,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

#[test]
fn sqlite_store_replays_committed_events() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let before = {
        let store = SqliteStore::open(&path).unwrap();
        store
            .execute(
                "race",
                Command::StartRace {
                    config: config(),
                    at: 0,
                },
            )
            .unwrap();
        store
            .execute("race", Command::AdvanceRace { to: 10 })
            .unwrap();
        store
            .execute(
                "race",
                Command::SensorTriggered {
                    lane: 1,
                    at: 110,
                    edge: SignalEdge::Rising,
                },
            )
            .unwrap()
    };
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.load("race").unwrap(), before);
}

#[test]
fn failed_command_leaves_protocol_and_state_unchanged_atomically() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let before = store
        .execute(
            "race",
            Command::StartRace {
                config: config(),
                at: 0,
            },
        )
        .unwrap();
    let events_before = store.events("race").unwrap();

    assert!(matches!(
        store.execute(
            "race",
            Command::SensorTriggered {
                lane: 3,
                at: 20,
                edge: SignalEdge::Rising,
            },
        ),
        Err(StoreError::Domain(DomainError::InvalidLane))
    ));
    assert_eq!(store.events("race").unwrap(), events_before);
    assert_eq!(store.load("race").unwrap(), before);
    assert!(matches!(before.phase, RacePhase::Starting { .. }));
}

#[test]
fn concurrent_writers_are_contiguous_without_lost_events() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    store
        .execute(
            "race",
            Command::StartRace {
                config: config(),
                at: 0,
            },
        )
        .unwrap();
    store
        .execute("race", Command::AdvanceRace { to: 10 })
        .unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let mut threads = vec![];
    for lane in 1..=2 {
        let barrier = barrier.clone();
        let store = store.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            store
                .execute(
                    "race",
                    Command::SensorTriggered {
                        lane,
                        at: 110,
                        edge: SignalEdge::Rising,
                    },
                )
                .unwrap();
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }

    let state = store.load("race").unwrap();
    assert_eq!(state.lane(1).unwrap().laps, 1);
    assert_eq!(state.lane(2).unwrap().laps, 1);
    let connection = Connection::open(&path).unwrap();
    let sequences: Vec<i64> = connection
        .prepare("SELECT sequence FROM race_events WHERE race_id = 'race' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(sequences, (1..=7).collect::<Vec<_>>());
    let malformed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM race_events WHERE event_type = '' OR schema_version != 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(malformed, 0);
    assert!(
        connection
            .execute(
                "UPDATE race_events SET payload = '{}' WHERE race_id = 'race' AND sequence = 1",
                []
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM race_events WHERE race_id = 'race' AND sequence = 1",
                []
            )
            .is_err()
    );
}

#[test]
fn store_cli() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("lapx.db");
    let input = dir.path().join("start.json");
    let mut race_config = config();
    race_config.finish_condition = FinishCondition::Laps(1);
    std::fs::write(
        &input,
        serde_json::json!({ "race_id": "race", "at": 0, "config": race_config }).to_string(),
    )
    .unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args(["start", "--json", input.to_str().unwrap()])
        .env("LAPX_DB", &db)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: RaceState = serde_json::from_slice(&output.stdout).unwrap();
    assert!(matches!(state.phase, RacePhase::Starting { .. }));

    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args(["advance", "--json", "-"])
        .env("LAPX_DB", &db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"race_id":"race","to":10}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let state: RaceState = serde_json::from_slice(&output.stdout).unwrap();
    assert!(matches!(state.phase, RacePhase::Running { .. }));

    let sensor = dir.path().join("sensor.json");
    std::fs::write(
        &sensor,
        r#"{"race_id":"race","lane":1,"at":110,"edge":"rising"}"#,
    )
    .unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args(["sensor", "--json", sensor.to_str().unwrap()])
        .env("LAPX_DB", &db)
        .output()
        .unwrap();
    assert!(output.status.success());
    let state: RaceState = serde_json::from_slice(&output.stdout).unwrap();
    assert!(matches!(state.phase, RacePhase::Finished { .. }));

    let correction = dir.path().join("correct.json");
    std::fs::write(
        &correction,
        r#"{"race_id":"race","lane":2,"delta_thousandths":500,"at":111}"#,
    )
    .unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args(["correct", "--json", correction.to_str().unwrap()])
        .env("LAPX_DB", &db)
        .output()
        .unwrap();
    assert!(output.status.success());
    let state: RaceState = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state.lane(2).unwrap().corrected_laps_thousandths, 500);
}
