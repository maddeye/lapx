use lapx::{
    domain::{ChaosSource, Command, RaceConfig, SignalEdge},
    store::SqliteStore,
};
use serde::Deserialize;
use std::{
    env, fs,
    io::{self, Read},
    process,
};

#[derive(Deserialize)]
struct StartInput {
    race_id: String,
    at: u64,
    config: RaceConfig,
}
#[derive(Deserialize)]
struct AdvanceInput {
    race_id: String,
    to: u64,
}
#[derive(Deserialize)]
struct SensorInput {
    race_id: String,
    lane: u8,
    at: u64,
    edge: SignalEdge,
}
#[derive(Deserialize)]
struct CorrectInput {
    race_id: String,
    lane: u8,
    delta_thousandths: i64,
    at: u64,
}
#[derive(Deserialize)]
struct TimedInput {
    race_id: String,
    at: u64,
}
#[derive(Deserialize)]
struct ChaosInput {
    race_id: String,
    source: ChaosSource,
    at: u64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lapxctl: {error}");
        process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 3 || args[1] != "--json" {
        return Err(
            "usage: lapxctl <start|advance|sensor|correct|pause|resume|chaos> --json <file|->"
                .into(),
        );
    }
    let json = if args[2] == "-" {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| error.to_string())?;
        input
    } else {
        fs::read_to_string(&args[2]).map_err(|error| error.to_string())?
    };
    let (race_id, command) = parse_command(&args[0], &json)?;
    let database = env::var_os("LAPX_DB").unwrap_or_else(|| "lapx.db".into());
    let state = SqliteStore::open(database)
        .map_err(|error| error.to_string())?
        .execute(&race_id, command)
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&state).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn parse_command(name: &str, json: &str) -> Result<(String, Command), String> {
    match name {
        "start" => {
            let input: StartInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            Ok((
                input.race_id,
                Command::StartRace {
                    config: input.config,
                    at: input.at,
                },
            ))
        }
        "advance" => {
            let input: AdvanceInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            Ok((input.race_id, Command::AdvanceRace { to: input.to }))
        }
        "sensor" => {
            let input: SensorInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            Ok((
                input.race_id,
                Command::SensorTriggered {
                    lane: input.lane,
                    at: input.at,
                    edge: input.edge,
                },
            ))
        }
        "correct" => {
            let input: CorrectInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            Ok((
                input.race_id,
                Command::CorrectLaps {
                    lane: input.lane,
                    delta_thousandths: input.delta_thousandths,
                    at: input.at,
                },
            ))
        }
        "pause" | "resume" => {
            let input: TimedInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            let command = if name == "pause" {
                Command::PauseRace { at: input.at }
            } else {
                Command::ResumeRace { at: input.at }
            };
            Ok((input.race_id, command))
        }
        "chaos" => {
            let input: ChaosInput =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            Ok((
                input.race_id,
                Command::TriggerChaos {
                    source: input.source,
                    at: input.at,
                },
            ))
        }
        _ => Err(format!("unknown command {name}")),
    }
}
