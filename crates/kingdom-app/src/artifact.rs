//! `/plan/{id}/artifact/{*path}`: serving a file a plan's work left behind.
//!
//! The read half of [`kingdom_core::ToolArtifact`]. A tool records *where* it
//! put a file; this hands that file back to the chamber, so a screenshot the
//! court took is a picture the King can see rather than a path he cannot open.
//!
//! # Why a route rather than the bytes on the plan
//!
//! The plan already travels to the browser over [`crate::watch`], so inlining
//! the bytes would be fewer moving parts. It would also be wrong three times
//! over: `store.rs` strips images before writing, so the picture would vanish
//! on reload; `llm/copilot.rs` sends every image on a settled call to a model
//! that can see, so every capture would start costing context whether or not
//! the model asked to look; and the wire carries *whole plans* on every change,
//! so one screenshot would be re-sent for the life of the conversation.
//!
//! The file is already on disk inside the plan's workspace, where the path
//! boundary already applies. Naming it costs a few bytes in the record and
//! nothing anywhere else.
//!
//! # The boundary
//!
//! Every path arrives from a browser and is resolved through the plan's own
//! [`Sandbox`] -- the same call every tool makes. This route is the one place
//! in Kingdom where an outsider names a file and the server opens it, so it
//! refuses rather than guesses: outside the workspace is a refusal, a media
//! type `read_image` would not accept is a refusal, and nothing here writes,
//! lists a directory, or follows a plan that does not exist.
//!
//! # What is where
//!
//! [`ROUTE`] and [`url`] are compiled into both targets, because the browser is
//! what builds the link and the server is what answers it -- one definition,
//! and neither side can drift from the other. The handler itself is ssr-only.

#[cfg(feature = "ssr")]
use axum::extract::Path as UrlPath;
#[cfg(feature = "ssr")]
use axum::http::{header, StatusCode};
#[cfg(feature = "ssr")]
use axum::response::{IntoResponse, Response};

#[cfg(feature = "ssr")]
use crate::tools::{read_image, Sandbox};

/// The path the chamber fetches a plan's files from.
///
/// A wildcard tail because artifact paths are workspace-relative and may have
/// directories in them; the `Sandbox` is what makes that safe rather than the
/// shape of the route.
pub const ROUTE: &str = "/plan/{id}/artifact/{*path}";

/// Builds the URL the conversation should point an `<img>` at.
///
/// Beside the route it has to match rather than in the view, so the two cannot
/// drift into disagreement -- the failure of which is a broken image nobody
/// notices until a user does.
pub fn url(plan: &kingdom_core::PlanId, path: &str) -> String {
    // Only what would otherwise change the URL's shape. A general encoder would
    // escape the separators too, and the route's tail is expected to keep them.
    let escaped = path
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace(' ', "%20");
    format!("/plan/{plan}/artifact/{escaped}")
}

#[cfg(feature = "ssr")]
pub async fn serve(UrlPath((id, path)): UrlPath<(String, String)>) -> Response {
    let plan = kingdom_core::PlanId::new(id);

    // A plan Kingdom does not know is a 404 rather than a refusal: there is no
    // workspace to judge the path against, so there is nothing to say about it.
    let Some(plan) = crate::api::snapshot(&plan) else {
        return (StatusCode::NOT_FOUND, "No such plan.").into_response();
    };

    from_workspace(&plan.workspace, &path).await
}

/// Everything the route decides once the plan's workspace is known.
///
/// Split from [`serve`] so the boundary can be tested against a real directory
/// without a kingdom in global state -- the decisions worth pinning are all
/// here, and reaching into the server's `OnceLock` to reach them would make
/// each test depend on what the last one filed.
#[cfg(feature = "ssr")]
async fn from_workspace(workspace: &kingdom_core::Workspace, path: &str) -> Response {
    let shop = Sandbox::new(workspace.clone());

    // The same boundary every tool is held to, called the same way. Outside the
    // workspace is 403 and not 404 deliberately: the distinction costs nothing
    // here and a silent 404 would hide the one bug this route could have.
    let Ok(resolved) = shop.resolve(path) else {
        return (
            StatusCode::FORBIDDEN,
            "That path is outside this plan's workspace.",
        )
            .into_response();
    };

    // Only what `read_image` would accept. Sharing the list rather than
    // repeating it is what stops the two drifting into disagreeing about what
    // an image is -- and this route must never become a general file server for
    // a plan's workspace, which is what an open-ended media type would make it.
    let Some(media) = read_image::media_type(&resolved) else {
        return (
            StatusCode::FORBIDDEN,
            "Only images are served from a plan's workspace.",
        )
            .into_response();
    };

    // Missing is ordinary, not exceptional: a merged or archived plan has had
    // its worktree cleared away, and the chamber renders that as words rather
    // than as a broken image.
    let Ok(bytes) = tokio::fs::read(&resolved).await else {
        return (StatusCode::NOT_FOUND, "That file is no longer there.").into_response();
    };

    (
        [
            (header::CONTENT_TYPE, media),
            // The names carry a nanosecond serial, so a given URL is the same
            // bytes forever and re-rendering a long transcript costs one
            // request per picture rather than one per scroll.
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// A 1x1 transparent PNG -- a real one, so what is served could actually be
    /// decoded by the browser this exists for.
    const A_REAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// Asks for a path inside a workspace, the way the route would once the
    /// plan has been looked up.
    async fn ask(root: &std::path::Path, path: &str) -> (StatusCode, String, Vec<u8>) {
        let workspace = kingdom_core::Workspace::in_place(root.to_string_lossy());
        let response = from_workspace(&workspace, path).await;

        let status = response.status();
        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body")
            .to_vec();

        (status, media, body)
    }

    /// The happy path, and the assertion that matters most: what comes back is
    /// the file that went in, declared as what it is.
    #[tokio::test]
    async fn a_picture_in_the_workspace_is_served() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), A_REAL_PNG).unwrap();

        let (status, media, body) = ask(dir.path(), "shot.png").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(media, "image/png");
        assert_eq!(body, A_REAL_PNG, "the bytes served must be the bytes saved");
    }

    /// The reason this module is written the way it is. A path from a browser
    /// is the one place an outsider names a file the server will open, so the
    /// boundary is asserted here as well as in `tools::Sandbox`.
    #[tokio::test]
    async fn a_path_that_leaves_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        for escape in ["../secrets.png", "a/../../secrets.png"] {
            let (status, ..) = ask(dir.path(), escape).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{escape} leaves the workspace and must be refused"
            );
        }
    }

    /// Not a general file server. A plan's workspace is a whole checkout, and
    /// this route must not become a way to read `.env` out of it.
    #[tokio::test]
    async fn something_that_is_not_an_image_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secrets.env"), b"TOKEN=hunter2").unwrap();

        let (status, _, body) = ask(dir.path(), "secrets.env").await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            !String::from_utf8_lossy(&body).contains("hunter2"),
            "a refusal must not serve the file it refused"
        );
    }

    /// A merged or archived plan has had its worktree cleared away, so this is
    /// the *expected* state for an old transcript rather than a fault. The
    /// chamber turns it into words; here it is simply a 404.
    #[tokio::test]
    async fn a_file_that_is_gone_is_not_found() {
        let dir = tempfile::tempdir().unwrap();

        let (status, ..) = ask(dir.path(), "cleared-away.png").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// A plan Kingdom has never heard of has no workspace to judge a path
    /// against, so there is nothing to say about the path. Through the handler
    /// proper, because looking the plan up is the step being pinned.
    #[tokio::test]
    async fn an_unknown_plan_is_not_found() {
        let response = serve(UrlPath(("no-such-plan".into(), "shot.png".into()))).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The URL builder and the route must agree, and a name with a space in it
    /// must not silently become two path segments.
    #[test]
    fn the_url_matches_the_route_it_is_built_for() {
        let built = url(&kingdom_core::PlanId::new("plan-1"), "shot.png");
        assert_eq!(built, "/plan/plan-1/artifact/shot.png");
        assert!(ROUTE.starts_with("/plan/"), "the two must share a prefix");

        assert_eq!(
            url(&kingdom_core::PlanId::new("plan-1"), "a shot.png"),
            "/plan/plan-1/artifact/a%20shot.png"
        );
    }
}
