#[cfg(feature = "gpio")]
use lapx::hardware::gpio::{GpioPowerOutput, GpioTimingSource};
use lapx::{
    hardware::HardwareConfig,
    http::{local_server_router, public_router},
    runtime::RaceRuntime,
    store::SqliteStore,
};
#[cfg(not(feature = "gpio"))]
use std::io;
use std::{env, error::Error, net::SocketAddr};
use tokio::net::TcpListener;

/// Parses LAPX_LOCAL_BIND and rejects any non-loopback address before binding:
/// the local surface carries mutating and debug routes.
fn local_bind_addr(value: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = value
        .parse()
        .map_err(|error| format!("LAPX_LOCAL_BIND {value:?} is not a socket address: {error}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "LAPX_LOCAL_BIND {value:?} must be a loopback address; the local surface exposes mutating routes"
        ));
    }
    Ok(addr)
}

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
    let local_addr = local_bind_addr(&local_bind)?;
    let public_addr: SocketAddr = public_bind.parse().map_err(|error| {
        format!("LAPX_PUBLIC_BIND {public_bind:?} is not a socket address: {error}")
    })?;
    let local = TcpListener::bind(local_addr).await?;
    let public = TcpListener::bind(public_addr).await?;
    tokio::try_join!(
        axum::serve(local, local_server_router(runtime.clone())),
        axum::serve(public, public_router(runtime)),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::local_bind_addr;

    #[test]
    fn local_bind_rejects_non_loopback() {
        assert!(local_bind_addr("127.0.0.1:3000").is_ok());
        assert!(local_bind_addr("[::1]:3000").is_ok());
        assert!(local_bind_addr("0.0.0.0:3000").is_err());
        assert!(local_bind_addr("192.168.1.10:3000").is_err());
        assert!(local_bind_addr("[::]:3000").is_err());
        assert!(local_bind_addr("not-an-address").is_err());
    }
}
