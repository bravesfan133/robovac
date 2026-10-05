//! The single upstream consumer of Valetudo's map event stream.
//!
//! Valetudo caps `/state/map/sse` at five concurrent clients and exposes the
//! map it just rendered. Opening one stream per browser would burn that budget
//! and fail for no good reason, so exactly one consumer runs here and the
//! resulting state change is fanned out to browsers over our own `/events`
//! endpoint.
//!
//! Why bother when the poller already refreshes state? Because this reacts to
//! the map actually changing rather than to a timer, and because it reads the
//! map through `/state/map`, which does not contact the robot. The poller
//! remains the thing that keeps summary state fresh.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::broadcast;

use crate::cache::RobotCache;
use crate::sse::SseParser;
use crate::valetudo::{ApiError, Valetudo};

/// Coalescing window. The robot emits map events in bursts while it drives, and
/// each one costs a fetch plus a full SVG serialisation, so brief bursts are
/// collapsed into a single refresh.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Backoff bounds for reconnection. Starts at 1s, doubles to 30s.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Reconnect if nothing at all arrives for this long. Valetudo sends a
/// keep-alive every 5s, so silence means the connection is dead even though
/// nothing has errored, and this is the only way to notice.
const STALE_AFTER: Duration = Duration::from_secs(30);

pub async fn run(
    state: Valetudo,
    cache: Arc<RobotCache>,
    updates: broadcast::Sender<serde_json::Value>,
) {
    let mut backoff = BACKOFF_MIN;

    loop {
        match consume(&state, &cache, &updates).await {
            Ok(()) => {
                // A clean end-of-stream is still a disconnection; reconnect
                // promptly rather than treating it as an error worth backing off.
                tracing::debug!("map event stream ended, reconnecting");
                backoff = BACKOFF_MIN;
            }
            Err(err) => {
                tracing::warn!(error = %err, "map event stream failed, will retry");
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// One connection's lifetime. Returns when the stream ends or errors.
async fn consume(
    state: &Valetudo,
    cache: &RobotCache,
    updates: &broadcast::Sender<serde_json::Value>,
) -> Result<(), ApiError> {
    let response = state.map_events().await?;
    if !response.status().is_success() {
        return Err(ApiError::Status {
            status: response.status().as_u16(),
            detail: Some("map event stream refused".into()),
        });
    }

    let mut parser = SseParser::new();
    let mut last_refresh = Instant::now() - DEBOUNCE;
    let mut last_traffic = Instant::now();
    let mut dirty = false;

    let mut stream = response.bytes_stream();

    loop {
        // The timeout has to wrap the read itself: awaiting a dead stream
        // otherwise hangs forever, and nothing would ever notice.
        let next = match tokio::time::timeout(STALE_AFTER, stream.next()).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                return Err(ApiError::Transport(format!(
                    "no traffic for {}s, treating the stream as dead",
                    STALE_AFTER.as_secs()
                )))
            }
        };

        last_traffic = Instant::now();
        let chunk = next.map_err(|e| ApiError::Transport(e.to_string()))?;
        // Lossy rather than failing: a partial UTF-8 sequence at a chunk
        // boundary is normal and the rest arrives in the next chunk.
        let text = String::from_utf8_lossy(&chunk).into_owned();

        for event in parser.push(&text) {
            // Valetudo's map stream is uncommented; treat any event as "the map
            // may have changed" and let the fingerprint check decide whether
            // anything actually did.
            tracing::trace!(event = %event.name, "map event");
            dirty = true;
        }

        if dirty && last_refresh.elapsed() >= DEBOUNCE {
            refresh_map(state, cache, updates).await;
            last_refresh = Instant::now();
            dirty = false;
        }
    }

    let _ = last_traffic;

    Ok(())
}

/// Pull the cached map and fold it into the cache, notifying browsers only if
/// the geometry actually changed.
async fn refresh_map(
    state: &Valetudo,
    cache: &RobotCache,
    updates: &broadcast::Sender<serde_json::Value>,
) {
    let before = cache.map_version();

    match state.map().await {
        Ok(map) => cache.set_map(map),
        Err(err) => {
            // Not worth alarming anyone over: the poller will pick it up and
            // the browser keeps the map it already has.
            tracing::debug!(error = %err, "could not fetch map after event");
            return;
        }
    }

    if cache.map_version() != before {
        tracing::debug!(version = cache.map_version(), "map changed upstream");
        let _ = updates.send(crate::cache::broadcast_payload(cache));
    }
}
