//! Does a synthetic click reach a page that decides what it means on hover?
//!
//! This is the property the whole click path turns on, and it cannot be shown
//! without a real browser -- the bug it pins lives in the timing between two
//! CDP messages, which no unit test can observe. So it launches Chrome and is
//! `#[ignore]`d: `AGENTS.md` ยง5 promises the ordinary suite runs on a bare
//! machine with nothing installed, and that promise is worth more than having
//! these run by default.
//!
//! ```
//! cargo test -p kingdom-browser -- --ignored
//! ```
//!
//! The page is a `data:` URL rather than a file or a served fixture, so the
//! test needs no port, no temporary directory and no cleanup -- and cannot
//! collide with anything else on a shared machine.

use kingdom_browser::BrowserSessionManager;
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(15);

/// A page shaped like Kingdom's map: the click handler reads what a *separate,
/// delayed* channel last reported as hovered, rather than the event's own
/// target. On the map that channel is a Bevy observer behind a 50ms poll; here
/// it is a 30ms timer. Either way a click that arrives with the pointer move
/// finds nothing hovered.
fn hover_page() -> String {
    let html = "<!doctype html><title>hover</title>\
        <style>#box{width:300px;height:200px;background:#345}</style>\
        <div id=box></div><div id=log>nothing</div>\
        <script>\
        let hovered=null;\
        box.addEventListener('mouseover',()=>setTimeout(()=>hovered='box',30));\
        box.addEventListener('mouseout',()=>hovered=null);\
        document.addEventListener('click',()=>log.textContent=hovered?'selected':'cleared');\
        </script>";
    format!("data:text/html,{}", urlencoding(html))
}

fn urlencoding(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The fault the settle exists for.
///
/// Without it `click_at` moves and presses in one CDP batch, the page has not
/// yet run its hover handler, and the click selects nothing -- which is exactly
/// what every plan trying to click a city on the map ran into. Verified by
/// setting `HOVER_SETTLE` to zero, at which point this fails.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn a_click_at_a_point_reaches_a_page_that_reads_hover() {
    let browsers = BrowserSessionManager::new();
    let plan = "click-at-a-point";
    browsers
        .navigate(plan, &hover_page(), PATIENCE)
        .await
        .expect("the fixture page loads");

    // Inside #box, which stands 300x200 at the top left.
    browsers
        .click_at(plan, 150.0, 100.0, PATIENCE)
        .await
        .expect("the click is dispatched");

    let said = browsers
        .evaluate(
            plan,
            "document.getElementById('log').textContent",
            false,
            PATIENCE,
        )
        .await
        .expect("the page answers");
    assert!(said.contains("selected"), "the click did not land: {said}");
}

/// The same settle, on the selector path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn a_click_by_selector_reaches_a_page_that_reads_hover() {
    let browsers = BrowserSessionManager::new();
    let plan = "click-by-selector";
    browsers
        .navigate(plan, &hover_page(), PATIENCE)
        .await
        .expect("the fixture page loads");

    browsers
        .click(plan, "#box", false, PATIENCE)
        .await
        .expect("the click is dispatched");

    let said = browsers
        .evaluate(
            plan,
            "document.getElementById('log').textContent",
            false,
            PATIENCE,
        )
        .await
        .expect("the page answers");
    assert!(said.contains("selected"), "the click did not land: {said}");
}

/// A plan must not have to resize before its first screenshot is worth taking.
///
/// The unit test beside `DEFAULT_VIEWPORT` pins the intent; this pins that the
/// intent survives all the way into a launched Chrome, which is where the
/// previous default was actually observed to be wrong.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn the_browser_opens_wide_enough_to_show_an_unfolded_interface() {
    let browsers = BrowserSessionManager::new();
    let plan = "viewport";
    browsers
        .navigate(plan, "about:blank", PATIENCE)
        .await
        .expect("a blank page loads");

    let width: u32 = browsers
        .evaluate(plan, "innerWidth", false, PATIENCE)
        .await
        .expect("the page answers")
        .trim()
        .parse()
        .expect("innerWidth is a number");

    assert!(
        width >= 1250,
        "a browser {width}px wide folds Kingdom's own cities rail"
    );
}
