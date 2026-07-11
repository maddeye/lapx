use lapx::{http::router, runtime::RaceRuntime, store::SqliteStore};
use std::{env, error::Error};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let database = env::var_os("LAPX_DB").unwrap_or_else(|| "lapx.db".into());
    let runtime = RaceRuntime::new(SqliteStore::open(database)?, "race").await?;
    let listener = TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, router(runtime)).await?;
    Ok(())
}
