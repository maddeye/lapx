#[cfg(feature = "gpio")]
use lapx::hardware::gpio::{GpioPowerOutput, GpioTimingSource};
use lapx::{
    hardware::HardwareConfig,
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::SqliteStore,
};
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
    let local_bind = env::var("LAPX_LOCAL_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let public_bind = env::var("LAPX_PUBLIC_BIND").unwrap_or_else(|_| "0.0.0.0:3001".into());
    let local = TcpListener::bind(&local_bind).await?;
    let public = TcpListener::bind(&public_bind).await?;
    tokio::try_join!(
        axum::serve(local, local_router(runtime.clone())),
        axum::serve(public, public_router(runtime)),
    )?;
    Ok(())
}
