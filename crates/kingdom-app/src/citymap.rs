//! `/kingdom/map.json`: the manifest the map draws itself from.
//!
//! Server-only. The route and its URL live in [`kingdom_citymap::ROUTE`], named
//! by both the browser that fetches and the server that answers, so the two
//! cannot drift -- the same arrangement [`crate::artifact`] uses.
//!
//! # Why this is cached
//!
//! Building a manifest walks every file of every project. Measured over a real
//! dev folder -- five repositories, 2,117 files -- that is about 1.7 seconds and
//! 4.4 MB of JSON. Too slow to pay on every mount, and far too cheap to deserve
//! a background job with its own lifecycle, so it is simply memoised and
//! rebuilt when the kingdom it describes is no longer the kingdom that is open.
//!
//! The key is the kingdom's root path plus its city names. That deliberately
//! does *not* notice a file changing inside a project: the map draws the shape
//! of a codebase rather than its contents, and rescanning ~2,000 files because
//! one of them was saved would cost seconds to move a rooftop. Opening a
//! different folder, or a project appearing or disappearing, is what the map
//! actually needs to follow.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Mutex;

/// The rendered manifest, and the kingdom it was built for.
static CACHE: Mutex<Option<(String, String)>> = Mutex::new(None);

/// What the cached manifest was built from.
///
/// Cheap to compute and stable across rescans, since `scan_kingdom` sorts its
/// cities by name.
fn key(kingdom: &kingdom_core::Kingdom) -> String {
    let mut key = String::with_capacity(64);
    key.push_str(&kingdom.root);
    for city in &kingdom.cities {
        key.push('\u{1f}');
        key.push_str(&city.name);
    }
    key
}

/// Serves the map for whichever kingdom is currently open.
pub async fn serve() -> Response {
    let Some(kingdom) = crate::api::kingdom_snapshot() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no kingdom is open").into_response();
    };
    if kingdom.cities.is_empty() {
        return (StatusCode::NOT_FOUND, "this kingdom has no cities").into_response();
    }

    let wanted = key(&kingdom);
    if let Ok(cache) = CACHE.lock() {
        if let Some((cached_key, json)) = cache.as_ref() {
            if cached_key == &wanted {
                return json_response(json.clone());
            }
        }
    }

    // Built outside the lock: this is seconds of filesystem work, and holding
    // the mutex across it would park every other request behind it.
    let Some(manifest) = kingdom_citymap::manifest_for(&kingdom) else {
        return (
            StatusCode::NOT_FOUND,
            "none of this kingdom's cities could be read",
        )
            .into_response();
    };
    let json = match serde_json::to_string(&manifest) {
        Ok(json) => json,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the map could not be encoded: {error}"),
            )
                .into_response();
        }
    };

    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((wanted, json.clone()));
    }
    json_response(json)
}

fn json_response(json: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], json).into_response()
}
