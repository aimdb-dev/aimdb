//! Plain-text HTTP polling of current record values, without mirrored state.
use std::{sync::Arc, time::Duration};

use aimdb_core::{buffer::BufferCfg, AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::WebSocketConnector;
use axum::{http::StatusCode, routing::get, Extension, Router};

// These types deliberately do not implement serde traits.
#[derive(Clone, Debug)]
struct Temperature(f32);
#[derive(Clone, Debug)]
struct Humidity(f32);
#[derive(Clone, Debug)]
struct WindowContact(bool);

fn current<T: Send + Clone + std::fmt::Debug + 'static>(
    db: &AimDb,
    key: &str,
) -> Result<T, StatusCode> {
    db.latest::<T>(key)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

async fn temperature(Extension(db): Extension<AimDb>) -> Result<String, StatusCode> {
    Ok(format!(
        "{:.1}",
        current::<Temperature>(&db, "room.temperature")?.0
    ))
}

async fn humidity(Extension(db): Extension<AimDb>) -> Result<String, StatusCode> {
    Ok(format!(
        "{:.1}",
        current::<Humidity>(&db, "room.humidity")?.0
    ))
}

async fn window(Extension(db): Extension<AimDb>) -> Result<String, StatusCode> {
    Ok(if current::<WindowContact>(&db, "room.window")?.0 {
        "1"
    } else {
        "0"
    }
    .into())
}

fn update(db: &AimDb, tick: u32) -> aimdb_core::DbResult<()> {
    db.produce(
        "room.temperature",
        Temperature(21.5 + (tick % 10) as f32 * 0.1),
    )?;
    db.produce("room.humidity", Humidity(45.0 + (tick % 20) as f32 * 0.5))?;
    db.produce("room.window", WindowContact((tick / 5) % 2 == 1))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Change to e.g. "0.0.0.0:8080" to allow LAN clients.
    let addr = "127.0.0.1:8080";
    let routes = Router::new()
        .route("/temperature", get(temperature))
        .route("/humidity", get(humidity))
        .route("/window", get(window));
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            WebSocketConnector::new()
                .bind(addr)
                .with_additional_routes(routes),
        );
    builder.configure::<Temperature>("room.temperature", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    builder.configure::<Humidity>("room.humidity", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    builder.configure::<WindowContact>("room.window", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    let (db, runner) = builder.build().await?;
    update(&db, 0)?;
    let server = tokio::spawn(runner.run());
    println!("Polling endpoints: http://{addr}/temperature, /humidity, /window");
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut tick = 0u32;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = interval.tick() => {
                update(&db, tick)?;
                tick = tick.wrapping_add(1);
            }
        }
    }
    server.abort();
    Ok(())
}
