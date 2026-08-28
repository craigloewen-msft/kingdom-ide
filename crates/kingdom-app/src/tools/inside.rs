//! Running a path-taking tool *inside* a sealed plan's own filesystem.
//!
//! # The hole this closes
//!
//! `read_file`, `patch`, `search` and `read_image` used to run in the server
//! process, on the King's machine, bounded only by the string comparison in
//! [`crate::tools::Sandbox::resolve`]. For a plan the King opened on his own
//! filesystem that is the honest boundary and nothing more is claimed. For a
//! **sealed** plan it was a promise the mechanism could not keep.
//!
//! A symlink *inside* the workspace pointing out of it defeats the check
//! completely, and not by cleverness: the path named is genuinely inside, so
//! `resolve` passes it, and the kernel then follows the link. Measured against
//! a real sealed plan, with a link at `<workspace>/innocent.txt`:
//!
//! ```text
//!   host-side std::fs::read : Ok("the King's private key\n")
//!   inside the namespace    : cat: /app/innocent.txt: No such file or directory
//! ```
//!
//! The agent can create that link itself -- `bash` runs inside and writing a
//! symlink needs no target -- and the workspace is bind-mounted, so it appears
//! on the host where the file tools would follow it. Both halves are ordinary;
//! only their meeting point was wrong.
//!
//! Inside the namespace the question does not arise. The link's target was
//! never mounted, so there is nothing to follow and the read fails closed. The
//! kernel enforces it, rather than a comparison we have to keep correct.
//!
//! # How it works
//!
//! The same trick `main.rs` already plays for `--relay`: re-enter *this binary*
//! inside the plan's namespaces, in a hidden mode. Here the mode is `--tool`,
//! it is given the tool's name and its JSON arguments, and it prints a
//! [`ToolOutcome`] as JSON on stdout.
//!
//! ```text
//!   server                          inside the plan's namespaces
//!   ------                          ----------------------------
//!   invoke("read_file", args)
//!     └─ run_inside(...)
//!          nsenter --mount --pid ──▶ /kingdom-helper --tool read_file '<args>'
//!                                      └─ the same Tool::run, on a filesystem
//!                                         where only mounted things exist
//!          ◀── ToolOutcome as JSON ───┘
//! ```
//!
//! `ToolOutcome` is `Serialize`/`Deserialize` already, because it is what the
//! chamber renders and what a plan's record keeps -- so the wire format is the
//! domain type itself and there is no second shape to keep in step.
//!
//! # Three things this deliberately does not do
//!
//! **It does not apply to every tool.** `bash`, `tmux` and the browser already
//! enter the namespace by their own route, and `think`, `propose_plan` and
//! `ask_user_question` touch the King's records rather than the plan's files --
//! running those inside would put them where the records are not. See
//! [`runs_inside`].
//!
//! **It does not apply to a plan that is not sealed.** `Shared` and `Isolated`
//! plans have no filesystem of their own to enter, so there is nothing to gain
//! and a subprocess to lose.
//!
//! **It does not fall back to the host.** A sealed plan whose helper will not
//! run is refused. Quietly running on the King's filesystem after he asked for
//! a machine of its own is the outcome the whole feature exists to prevent --
//! the same reasoning `bash` and `terminal.rs` already apply to the network.

use kingdom_core::ToolOutcome;
use serde_json::Value;

/// The flag that puts this binary into tool mode. See `main.rs`.
pub const FLAG: &str = "--tool";

/// How long a tool gets inside before it is called lost.
///
/// Generous, because a `search` over a large repository is legitimately slow,
/// and the tools this wraps have no timeout of their own -- they either finish
/// or they have hit something pathological. The bound exists so a wedged helper
/// cannot hang the turn forever, not to police ordinary work.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether this tool should be run inside the plan's own filesystem.
///
/// The list is the tools that **take a path and touch the filesystem**, and it
/// is written out rather than derived so that adding a tool is a decision
/// somebody makes on purpose. A tool missing from here still works -- it simply
/// runs on the host, which for a sealed plan is the weaker guarantee this
/// module exists to remove.
///
/// Everything absent is absent for a reason:
///
/// - `bash`, `tmux_run`, `tmux`, `browser_*`, `profile_*` already enter the
///   namespace themselves, by `enter_prefix`. Wrapping them would nest one
///   `nsenter` inside another.
/// - `think` touches nothing at all.
/// - `propose_plan`, `ask_user_question` and `spawn_agents` reach back into the
///   *server's* state -- the plan record, the question channel, the turn loop.
///   Inside the namespace that state is not there.
/// - `skill` reads the workspace but then hands back a command for the model to
///   run, and its discovery walks the same tree `search` does; it is left on
///   the host deliberately for now rather than half-moved.
pub fn runs_inside(tool: &str) -> bool {
    matches!(tool, "read_file" | "patch" | "search" | "read_image")
}

/// Runs one tool inside the plan's namespaces and brings its outcome back.
///
/// `Err` is for the *mechanism* failing -- no helper, `nsenter` would not
/// start, unreadable output. The caller turns that into a refusal rather than
/// falling back to the host, because a sealed plan that quietly ran on the
/// King's filesystem is the failure this exists to prevent.
pub async fn run_inside(
    tool: &str,
    input: &Value,
    shop: &crate::tools::Sandbox,
) -> Result<ToolOutcome, String> {
    let enter = crate::namespaces::enter_prefix(shop.plan());
    if enter.is_empty() {
        return Err(
            "this plan's own filesystem could not be entered, so nothing was run".to_string(),
        );
    }

    // Everything the far side needs to rebuild the same call, and no more. The
    // workspace is sent as `/app` -- inside the namespace that is a real mount
    // and the shorter of the two names, and `resolve` there needs no alias.
    //
    // The permissions travel because they *change which tool exists*: `patch`
    // under `Propose` may write only the plan's draft, and a helper that
    // defaulted to `Full` would hand a proposing plan the unrestricted editor.
    // See `tools::all`.
    let request = Request {
        tool: tool.to_string(),
        input: input.clone(),
        workspace: crate::namespaces::mount::WORKSPACE_AT.to_string(),
        permissions: shop.permissions(),
        plan: shop.plan().as_str().to_string(),
        clipboards: crate::tools::patch::clipboards_for(shop.plan().as_str()),
    };
    let Ok(encoded) = serde_json::to_string(&request) else {
        return Err("this call could not be described to the helper".to_string());
    };

    // The helper as it is named *inside* the root, not on the host: this argv
    // is executed after `nsenter` has entered the mount namespace, where the
    // King's `target/` directory is not mounted. See `mount::HELPER_AT`.
    let mut argv = enter;
    argv.push(crate::namespaces::mount::HELPER_AT.to_string());
    argv.push(FLAG.to_string());
    argv.push(encoded);

    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = command
        .spawn()
        .map_err(|e| format!("the helper could not be started: {e}"))?;

    let finished = tokio::time::timeout(PATIENCE, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "the helper did not finish within {} seconds",
                PATIENCE.as_secs()
            )
        })?
        .map_err(|e| format!("the helper could not be waited for: {e}"))?;

    if !finished.status.success() {
        let said = String::from_utf8_lossy(&finished.stderr);
        let said = said.trim().to_string();
        // The exec failing is worth naming as itself: `nsenter` reports a
        // missing helper as "failed to execute", which reads as a broken tool
        // rather than as a plan built without one.
        return Err(if said.is_empty() {
            format!("the helper exited with {}", finished.status)
        } else {
            format!("the helper exited with {}: {said}", finished.status)
        });
    }

    let answer: Answer = serde_json::from_slice(&finished.stdout).map_err(|e| {
        format!(
            "the helper's answer could not be read: {e}. It said: {}",
            String::from_utf8_lossy(&finished.stdout)
                .chars()
                .take(200)
                .collect::<String>()
        )
    })?;

    // Clipboards live in the *server's* memory, because they outlive any one
    // call: a `toClipboard` in one patch is pasted by a `fromClipboard` in the
    // next. The helper is a fresh process every time and cannot hold them, so
    // they travel out and back. Without this, moving code between two files --
    // the thing clipboards exist for -- would silently paste nothing.
    crate::tools::patch::store_clipboards(shop.plan().as_str(), answer.clipboards);

    Ok(answer.outcome)
}

/// What the server sends the helper: one tool call, whole.
///
/// One struct rather than a list of positional arguments because it crosses a
/// process boundary, where a silently reordered pair of strings is a bug no
/// compiler catches.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub tool: String,
    pub input: Value,
    pub workspace: String,
    pub permissions: crate::tools::Permissions,
    pub plan: String,
    /// The plan's clipboards, which `patch` may read and write. See
    /// [`Answer::clipboards`].
    pub clipboards: std::collections::HashMap<String, String>,
}

/// What the helper sends back: the outcome, and anything it changed that the
/// server must keep.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    pub outcome: ToolOutcome,
    /// The clipboards as they stand after the call.
    ///
    /// Returned rather than assumed unchanged: `patch` writes them, and the
    /// helper's copy dies with the process. See the note at the call site.
    pub clipboards: std::collections::HashMap<String, String>,
}

/// The whole of what the `--tool` process does, once `main` has recognised it.
///
/// Lives here rather than in `main.rs` so that the two halves of this
/// arrangement -- the side that sends and the side that receives -- can be read
/// together. `main.rs` keeps only the argument sniff, exactly as it does for
/// `--relay`.
///
/// The workspace is passed as its path and rebuilt into a [`Sandbox`] here.
/// Deliberately **not** carrying the plan's isolation across: inside the
/// namespace `/app` is a real mount, so `resolve` needs no alias and should
/// apply the plain boundary. It is also the honest description of this process,
/// which genuinely is on a filesystem where the workspace has one meaning.
pub async fn serve_one(encoded: &str) -> String {
    let request: Request = match serde_json::from_str(encoded) {
        Ok(request) => request,
        // Printed as a well-formed answer rather than a panic: the server reads
        // stdout, and a helper that died silently is indistinguishable from one
        // that could not be started.
        Err(e) => {
            return answer_of(
                ToolOutcome::Refused {
                    reason: format!("the helper could not read the call it was given: {e}"),
                },
                std::collections::HashMap::new(),
            )
        }
    };

    crate::tools::patch::store_clipboards(&request.plan, request.clipboards);

    // `already_inside`, which is what stops `invoke` sending this straight back
    // out to another helper: the plan is still sealed on this side of the
    // boundary, so the sealed test alone would recurse forever.
    //
    // Left at the default `Shared` isolation deliberately -- and it is the
    // truth here: this process really is standing on a filesystem where the
    // workspace has exactly one meaning, so `resolve` needs no `/app` alias and
    // should apply the plain boundary.
    let shop = crate::tools::Sandbox::new(kingdom_core::Workspace::in_place(&request.workspace))
        .for_plan(kingdom_core::PlanId::new(request.plan.clone()))
        .under(request.permissions)
        .already_inside();

    let outcome = crate::tools::invoke(&request.tool, request.input, &shop).await;

    answer_of(outcome, crate::tools::patch::clipboards_for(&request.plan))
}

/// One answer, as the line the server reads off stdout.
fn answer_of(
    outcome: ToolOutcome,
    clipboards: std::collections::HashMap<String, String>,
) -> String {
    let answer = Answer {
        outcome,
        clipboards,
    };
    serde_json::to_string(&answer).unwrap_or_else(|e| {
        // Every field is plain data, so this cannot realistically fail -- but a
        // panic here would be an empty stdout and an unreadable diagnosis.
        format!(
            "{{\"outcome\":{{\"Refused\":{{\"reason\":\"the helper could not describe its own answer: {e}\"}}}},\"clipboards\":{{}}}}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is the tools that touch the filesystem, and nothing else.
    ///
    /// Both halves matter and they fail in opposite directions. A file tool
    /// missing from it runs on the host, which is the hole this module exists
    /// to close. A tool wrongly *in* it is worse: `bash` already enters the
    /// namespace itself, so wrapping it would nest one `nsenter` inside
    /// another, and `propose_plan` would be sent to a process where the
    /// kingdom's records do not exist.
    #[test]
    fn only_the_tools_that_touch_files_run_inside() {
        for path_taking in ["read_file", "patch", "search", "read_image"] {
            assert!(
                runs_inside(path_taking),
                "{path_taking} reads or writes the plan's files and must be confined \
                 by the kernel rather than by a path comparison"
            );
        }

        for elsewhere in [
            // Already inside, by their own route.
            "bash",
            "tmux_run",
            "tmux",
            "browser_navigate",
            "browser_eval",
            // Touch the server's own state, which is not in there.
            "propose_plan",
            "ask_user_question",
            "spawn_agents",
            // Touches nothing at all.
            "think",
        ] {
            assert!(
                !runs_inside(elsewhere),
                "{elsewhere} must not be sent to the helper"
            );
        }
    }

    /// A request survives the trip out and back.
    ///
    /// It crosses a process boundary as JSON, so a field that failed to
    /// round-trip would not fail to compile -- it would silently arrive as its
    /// default. The permissions are the one that would hurt most: `Propose`
    /// arriving as `Full` hands a plan drawing up a proposal the unrestricted
    /// editor, which is the boundary `tools::all` exists to hold.
    #[test]
    fn a_call_survives_the_journey() {
        let request = Request {
            tool: "patch".to_string(),
            input: serde_json::json!({"path": "src/main.rs"}),
            workspace: crate::namespaces::mount::WORKSPACE_AT.to_string(),
            permissions: crate::tools::Permissions::Propose,
            plan: "plan-7".to_string(),
            clipboards: [("cb".to_string(), "held".to_string())]
                .into_iter()
                .collect(),
        };

        let text = serde_json::to_string(&request).expect("a call must serialise");
        let back: Request = serde_json::from_str(&text).expect("and come back");

        assert_eq!(back.tool, "patch");
        assert_eq!(back.permissions, crate::tools::Permissions::Propose);
        assert_eq!(back.plan, "plan-7");
        assert_eq!(back.clipboards.get("cb").map(String::as_str), Some("held"));
        assert_eq!(back.input["path"], "src/main.rs");
    }

    /// An answer does too, including the clipboards the far side changed.
    ///
    /// The clipboards are the reason this is not simply a `ToolOutcome`: they
    /// live in the server's memory because they outlive one call, and the
    /// helper is a fresh process every time. Dropped here, moving code between
    /// two files -- the thing clipboards exist for -- would paste nothing.
    #[test]
    fn an_answer_carries_the_clipboards_home() {
        let answer = Answer {
            outcome: ToolOutcome::done("Patched src/main.rs."),
            clipboards: [("moved".to_string(), "fn main() {}".to_string())]
                .into_iter()
                .collect(),
        };

        let text = serde_json::to_string(&answer).expect("an answer must serialise");
        let back: Answer = serde_json::from_str(&text).expect("and come back");

        assert_eq!(
            back.clipboards.get("moved").map(String::as_str),
            Some("fn main() {}")
        );
        assert!(matches!(back.outcome, ToolOutcome::Done { .. }));
    }

    /// A helper that cannot be reached is a refusal, never a fall-back.
    ///
    /// A plan with no namespace has an empty `enter_prefix`, and the one thing
    /// this must not then do is run the tool on the King's own filesystem. That
    /// is the same rule `bash` and `terminal.rs` apply to a network they cannot
    /// enter, and here it is the whole point: the King asked for a machine of
    /// its own.
    #[tokio::test]
    async fn a_plan_with_no_namespace_is_refused_rather_than_run_outside() {
        let nobody = kingdom_core::PlanId::new("plan-that-was-never-sealed");
        let shop = crate::tools::Sandbox::new(kingdom_core::Workspace::in_place("/dev/city"))
            .for_plan(nobody)
            .walled_off_by(kingdom_core::Isolation::Sealed);

        let refused = run_inside("read_file", &serde_json::json!({"path": "a.txt"}), &shop)
            .await
            .expect_err("there is no namespace to enter");

        assert!(
            refused.contains("could not be entered"),
            "the reason must name what failed: {refused}"
        );
    }
}
