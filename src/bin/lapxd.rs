#[cfg(feature = "gpio")]
use lapx::hardware::gpio::{GpioPowerOutput, GpioTimingSource};
use lapx::{hardware::HardwareConfig, http::router, runtime::RaceRuntime, store::SqliteStore};
#[cfg(not(feature = "gpio"))]
use std::io;
use std::{env, error::Error};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let database = env::var_os("LAPX_DB").unwrap_or_else(|| "lapx.db".into());
    let store = SqliteStore::open(database)?;
    let runtime = if let Ok(compact) = env::var("LAPX_HARDWARE") {
        let config = HardwareConfig::from_compact(&compact)?;
        #[cfg(feature = "gpio")]
        {
            let power = GpioPowerOutput::new(&config)?;
            RaceRuntime::with_hardware(
                store,
                "race",
                config.clone(),
                GpioTimingSource::new(config),
                power,
            )
            .await?
        }
        #[cfg(not(feature = "gpio"))]
        {
            let _ = (store, config);
            return Err(io::Error::other("LAPX_HARDWARE requires --features gpio").into());
        }
    } else {
        RaceRuntime::new(store, "race").await?
    };
    let listener = TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, router(runtime)).await?;
    Ok(())
}
