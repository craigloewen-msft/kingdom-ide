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

/// A closed session takes its Chrome and its profile with it.
///
/// The property this whole change turns on, and the one no unit test can show:
/// the unit tests can prove the map forgets a session, but only a real browser
/// can prove the *processes* are gone. They were not, before -- a plan that
/// took one screenshot in the morning still held nine processes and most of a
/// gigabyte at midnight, because nothing ever closed anything.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn closing_a_session_ends_its_processes_and_deletes_its_profile() {
    let browsers = BrowserSessionManager::new();
    let plan = "closes-cleanly";

    browsers
        .navigate(plan, "about:blank", PATIENCE)
        .await
        .expect("a blank page loads");
    assert_eq!(browsers.live().await, 1, "the browser should be open");

    let profile = profile_of(plan);
    assert!(
        profile.is_dir(),
        "the launch should have made {}",
        profile.display()
    );
    let processes = chrome_processes_using(&profile);
    assert!(
        !processes.is_empty(),
        "a launched browser should have processes"
    );

    browsers.close(plan).await;

    assert_eq!(browsers.live().await, 0, "the session should be forgotten");

    // Chrome's children do not all exit the instant the parent is asked to,
    // so this is given a moment rather than asserted on the same tick.
    for _ in 0..50 {
        if chrome_processes_using(&profile).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        chrome_processes_using(&profile).is_empty(),
        "closing left Chrome processes running: {:?}",
        chrome_processes_using(&profile)
    );
    assert!(
        !profile.exists(),
        "closing left the profile behind at {}",
        profile.display()
    );
}

/// The reaper closes a browser that has gone cold, and only that one.
///
/// The everyday mechanism: most plans neither settle promptly nor are watched,
/// so without this a browser opened once would live until the server stopped.
/// Real Chrome rather than a stub, because "the session is forgotten" and "the
/// processes are gone" are different claims and only the second one matters.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn the_reaper_closes_a_cold_browser_and_spares_a_busy_one() {
    let browsers = BrowserSessionManager::new();

    browsers
        .navigate("gone-cold", "about:blank", PATIENCE)
        .await
        .expect("a blank page loads");
    let cold_profile = profile_of("gone-cold");

    // Let one of them age past the window while the other is used at the end
    // of it, so the two differ only in when they were last touched.
    tokio::time::sleep(Duration::from_millis(600)).await;

    browsers
        .navigate("still-warm", "about:blank", PATIENCE)
        .await
        .expect("a blank page loads");
    let warm_profile = profile_of("still-warm");
    assert_eq!(browsers.live().await, 2, "both browsers should be open");

    let closed = browsers.reap_idle(Duration::from_millis(500)).await;

    assert_eq!(
        closed,
        vec!["gone-cold".to_string()],
        "the wrong set closed"
    );
    assert_eq!(browsers.live().await, 1, "exactly one should remain");

    for _ in 0..50 {
        if chrome_processes_using(&cold_profile).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        chrome_processes_using(&cold_profile).is_empty(),
        "the cold browser's processes are still running"
    );
    assert!(
        !chrome_processes_using(&warm_profile).is_empty(),
        "the reaper killed a browser that was just used"
    );

    browsers.close("still-warm").await;
}

/// Where a plan's profile lives, by the same rule `session.rs` uses.
///
/// Recomputed here rather than exported, so the test is checking the *observed*
/// directory on disk rather than trusting a value the code under test handed
/// it.
fn profile_of(plan: &str) -> std::path::PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    plan.hash(&mut hasher);
    std::path::Path::new("/tmp").join(format!("kingdom-chrome-{:016x}", hasher.finish()))
}

/// Every process holding this profile, read from `/proc`.
///
/// The substring search is deliberate and is the point of the test above.
/// Chrome rewrites its own `argv` into one contiguous string for its process
/// title, so a matcher that split on NUL finds nothing in a real Chrome -- which
/// is exactly the defect this test caught. Bounded at the end so a path is not
/// confused with a longer one that begins with it.
///
/// Linux-only, like the sweep it mirrors; on any other platform it reports
/// nothing and the assertions above become vacuous rather than false.
fn chrome_processes_using(profile: &std::path::Path) -> Vec<u32> {
    let wanted = format!("--user-data-dir={}", profile.display()).into_bytes();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            cmdline
                .windows(wanted.len())
                .enumerate()
                .filter(|(_, window)| *window == wanted.as_slice())
                .any(|(at, _)| {
                    matches!(cmdline.get(at + wanted.len()), None | Some(0) | Some(b' '))
                })
                .then_some(pid)
        })
        .collect()
}
