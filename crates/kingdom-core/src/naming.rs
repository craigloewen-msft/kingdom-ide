//! Turning human text into a name git will accept.
//!
//! Pure and wasm-safe, like [`crate::layout`] -- it is maths over a string. It
//! lives in core rather than the server because a plan's slug is part of the
//! domain: it is what the branch is cut from, and the browser will want it the
//! moment there is any affordance for renaming a plan.
//!
//! The hard requirement is `git check-ref-format`. Everything below that is
//! taste; everything at that level is a failed `git worktree add` in front of
//! the King if it is wrong.

/// Longest slug we will produce, in characters.
///
/// Long enough to stay recognisable in `git branch`, short enough that the
/// branch name does not wrap in a terminal beside the `kingdom/` prefix.
const MAX: usize = 32;

/// What an input with nothing usable in it becomes.
const EMPTY: &str = "plan";

/// A git-safe ref component derived from human text.
///
/// Lowercased, ASCII alphanumeric runs joined by `-`, truncated on a word
/// boundary. Guaranteed to be a legal ref component: never empty, never
/// starting or ending with `-` or `.`, never containing `..`, `@{`, or any of
/// git's reserved characters, and never ending `.lock`.
///
/// Non-ASCII is dropped rather than transliterated. git would accept the bytes,
/// but a branch name is typed, tab-completed and pasted into shells, and a
/// half-transliterated one is worse than a short honest one. A decree with no
/// ASCII at all falls back to `"plan"`, which is the caller's cue that
/// uniqueness has to come from somewhere else.
pub fn slugify(text: &str) -> String {
    // One pass: every run of unusable bytes collapses to a single separator.
    // Building with the separator *deferred* means trailing dashes never get
    // written in the first place, rather than being trimmed off afterwards.
    let mut out = String::with_capacity(MAX);
    let mut pending_dash = false;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
            if out.chars().count() >= MAX {
                break;
            }
        } else {
            pending_dash = true;
        }
    }

    // Truncating mid-word leaves a fragment that reads like a typo, so back off
    // to the last separator -- unless that would throw away most of the name.
    if out.chars().count() >= MAX {
        if let Some((head, _)) = out.rsplit_once('-') {
            if head.chars().count() >= MAX / 2 {
                out.truncate(head.len());
            }
        }
    }

    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return EMPTY.to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test that matters: whatever the King types, git must accept the
    /// result. Every case here is a decree somebody could plausibly issue, and
    /// a failure is a refused decree rather than a cosmetic wart.
    ///
    /// The assertions restate `git check-ref-format`'s rules rather than
    /// pinning exact strings, so the slug's *taste* can change without
    /// rewriting the test, but its *safety* cannot regress.
    #[test]
    fn slugify_always_yields_a_legal_ref_component() {
        let hostile = [
            "",
            "   ",
            "---",
            "...",
            "..",
            "@{",
            "-leading and trailing-",
            ".hidden.",
            "index.lock",
            "refs/heads/nested",
            "punctuation!!!___runs???everywhere",
            "CAPITALS And Spaces",
            "🔥🔥🔥",
            "日本語のみ",
            "café naïve résumé",
            "tidy~the^sidebar:now?*[]\\",
            &"a very long decree about refactoring the entire rendering layer ".repeat(7),
        ];

        for input in hostile {
            let slug = slugify(input);

            assert!(!slug.is_empty(), "empty slug from {input:?}");
            assert!(
                slug.chars().count() <= MAX,
                "slug {slug:?} from {input:?} is longer than {MAX}"
            );
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "slug {slug:?} from {input:?} has characters outside [a-z0-9-]"
            );
            assert!(
                !slug.starts_with('-') && !slug.ends_with('-'),
                "slug {slug:?} from {input:?} starts or ends with a dash"
            );
            assert!(
                !slug.starts_with('.') && !slug.ends_with('.') && !slug.contains(".."),
                "slug {slug:?} from {input:?} has a dot problem"
            );
            assert!(
                !slug.contains("@{") && !slug.ends_with(".lock"),
                "slug {slug:?} from {input:?} hits a git reserved form"
            );
        }
    }

    /// The everyday case, pinned so the slug stays something a human recognises
    /// as their own decree rather than merely something git tolerates.
    #[test]
    fn slugify_reads_like_the_decree_it_came_from() {
        assert_eq!(slugify("Tidy the sidebar"), "tidy-the-sidebar");
        assert_eq!(
            slugify("Fix the parser's off-by-one"),
            "fix-the-parser-s-off-by-one"
        );

        // Truncation lands on a word boundary, not mid-word.
        let long = slugify("Rewrite the entire rendering layer from scratch");
        assert!(!long.ends_with('-'));
        assert!(
            long.split('-')
                .all(|w| "rewrite the entire rendering layer from scratch"
                    .split(' ')
                    .any(|orig| orig == w)),
            "{long:?} contains a truncated word fragment"
        );
    }
}
