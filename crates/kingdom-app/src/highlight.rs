//! Syntax colour: one file's lines, split into runs of one kind each.
//!
//! Server-only, and that is the design rather than an implementation detail.
//! [`crate::review::source`] already builds the [`SourceText`] the panel
//! renders, so tokenising *there* means the browser receives spans it can paint
//! directly and no syntax definition, dump or regex engine ever enters the wasm
//! bundle. The workspace manifest goes to considerable trouble over that bundle's
//! size; this keeps out of its way entirely.
//!
//! It is also the shape already in use: `review.rs` computes the emphasis spans
//! of a diff row server-side and `diff_view.rs` paints them. A source line is
//! the same arrangement with a different question asked of each run.
//!
//! # What this is not
//!
//! Not a language server, and not an accurate one. It folds the hundreds of
//! scopes a syntax definition distinguishes down to [`Token`]'s seven, because
//! the panel exists to be *skimmed* -- comments receding and strings separating
//! from code is the whole of the value, and a reader is not helped by a
//! different colour for a lifetime than for a keyword.
//!
//! # The guards, and why they are not optional
//!
//! Highlighting is the one thing in the read path whose cost is not bounded by
//! the guards `review::source` already applies. Those are stated in bytes and in
//! rows; this is quadratic in the width of a *line*, which neither of them
//! constrains. Measured on this workspace, 500 lines of a given width:
//!
//! | width  |  time |
//! |--------|-------|
//! |   200  | 0.9 s |
//! |   500  | 4.8 s |
//! |  1000  | 19.9s |
//! |  2000  | 35.3s |
//!
//! The case that matters is not synthetic. A minified bundle is *one line* of a
//! million characters: it is comfortably under `MOST_BYTES`, is one row against
//! a `MOST_ROWS` of four thousand, and so passes every existing guard. A 1.4 MB
//! one tokenised to **252,329 spans in 3.4 seconds** -- a quarter of a million
//! DOM nodes posted into the panel. This repository vendors minified JavaScript
//! itself, so this is a file the King can really open.
//!
//! So there are two limits, and both degrade the same way: to *plain, but
//! correct*. An uncoloured file is exactly what the panel showed before this
//! module existed, which makes falling back to it a non-event rather than a
//! failure worth reporting.

use kingdom_core::{CodeSpan, SourceLine, Token};
use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// How wide a line may be before it is taken as data rather than as code.
///
/// A line longer than this is emitted as one plain span without being parsed at
/// all -- which is what defends the minified-bundle case in the module note,
/// where it turned 252,329 spans and 3.4 seconds into one span and 60
/// microseconds.
///
/// 1,000 columns is far outside anything written to be read: the widest line in
/// this entire repository is 437 characters, and the 99th percentile across its
/// 63,310 lines of Rust is 91. Something wider is a bundle, a data table or a
/// base64 blob, and none of those is improved by colour.
const MOST_COLUMNS: usize = 1_000;

/// How many characters may be parsed for one file before the rest is left
/// plain.
///
/// The column cap alone bounds *one* line; this bounds the file. Without it a
/// pathological-but-legal file -- thousands of lines each just under the column
/// cap -- still costs tens of seconds, because the per-line cost is quadratic
/// and four thousand of them are allowed through. Measured: a synthetic
/// 4,000-line file of ~430-character lines took ~15 seconds with only the column
/// cap in place.
///
/// The number is a budget rather than a boundary: ordinary source spends a small
/// fraction of it (`model.rs`, the largest file here at 4,735 lines, spends
/// ~196,000) and never notices, while a file built to be expensive stops costing
/// at a fixed point. When it runs out the remaining lines are plain, and the
/// King sees a file that is coloured at the top and not at the bottom -- which
/// is worth more than a file that took half a minute to appear.
const MOST_WORK: usize = 600_000;

/// The syntaxes, loaded once.
///
/// `bat`'s set rather than syntect's own, because the defaults have **no
/// TypeScript, no SCSS and no TOML** -- three of the languages this repository
/// is itself written in, which makes the default set indefensible here.
///
/// Loaded from a binary dump and cached for the life of the process: measured at
/// ~830 microseconds, paid once on the first file opened rather than on every
/// read.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// Picks a syntax for a path, or `None` to leave the file plain.
///
/// Extension first, then the whole name -- which is what catches `Makefile`,
/// `Dockerfile` and the dotfiles that have no extension to look at.
/// [`kingdom_core::Language::from_path`] is deliberately *not* consulted: it
/// sorts files into eleven buckets for colouring a map, so it cannot tell
/// TypeScript from Svelte, and syntect needs the exact grammar.
fn syntax_for(path: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = path.rsplit('/').next().unwrap_or(path);

    if let Some((stem, ext)) = name.rsplit_once('.') {
        if !stem.is_empty() {
            if let Some(found) = set.find_syntax_by_extension(ext) {
                return Some(found);
            }
        }
    }

    // `Makefile`, `Dockerfile`, `.gitignore`: the name *is* the type. Also the
    // fallback for `.rs` if an extension lookup somehow missed.
    set.find_syntax_by_extension(name)
        .or_else(|| set.find_syntax_by_token(name))
}

/// Folds one of syntect's scopes down to one of ours.
///
/// The scope stack is innermost-last, so this walks it backwards and takes the
/// first thing it recognises: in `meta.function-call` wrapping
/// `variable.function`, the inner answer is the specific one and the right one.
///
/// The prefixes are ordered, and the order is the whole of the logic --
/// `constant.numeric` must be tried before `constant`, and `entity.name.function`
/// before `entity.name`, or the general answer swallows the specific one.
fn classify(stack: &ScopeStack) -> Token {
    for scope in stack.scopes.iter().rev() {
        if let Some(token) = token_for(*scope) {
            return token;
        }
    }
    Token::Plain
}

fn token_for(scope: Scope) -> Option<Token> {
    // `build_string` gives the dotted name; syntect's `Debug` wraps it in angle
    // brackets, which would break every `starts_with` below.
    let name = scope.build_string();

    const KINDS: [(&str, Token); 16] = [
        ("comment", Token::Comment),
        ("string", Token::Str),
        ("constant.numeric", Token::Number),
        ("constant.language", Token::Number),
        ("constant.character", Token::Str),
        ("keyword", Token::Keyword),
        ("storage", Token::Keyword),
        ("entity.name.function", Token::Function),
        ("support.function", Token::Function),
        ("meta.function-call", Token::Function),
        ("entity.name", Token::Type),
        ("entity.other.attribute-name", Token::Type),
        ("support.type", Token::Type),
        ("support.class", Token::Type),
        ("variable.annotation", Token::Type),
        ("constant", Token::Number),
    ];

    KINDS
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map(|(_, token)| *token)
}

/// Splits a file into numbered, coloured lines.
///
/// `lines` is what `review::source` has already cut to its row cap, so this
/// never sees more rows than the panel will draw. The 1-based numbering is the
/// caller's, for the reason its own note gives: a truncated file still has to
/// number honestly.
///
/// Never fails. A language nothing is known about, a line too wide to parse, a
/// budget run out and a syntax that errors mid-file all produce plain lines,
/// because a file the King cannot read is a far worse outcome than one he reads
/// in a single colour.
pub fn lines(path: &str, lines: &[&str]) -> Vec<SourceLine> {
    let numbered = |spans_of: &mut dyn FnMut(usize, &str) -> Vec<CodeSpan>| {
        lines
            .iter()
            .enumerate()
            .map(|(i, text)| SourceLine {
                number: (i + 1) as u32,
                spans: spans_of(i, text),
            })
            .collect()
    };

    let Some(syntax) = syntax_for(path) else {
        return numbered(&mut |_, text| plain(text));
    };

    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut spent = 0usize;

    numbered(&mut |_, text| {
        // Both guards, before any work is done on the line. Note that the parse
        // state is deliberately *not* advanced for a skipped line: the state
        // machine is then out of step with the file, which is why a spent budget
        // leaves everything after it plain rather than resuming later with
        // confident nonsense.
        if text.len() > MOST_COLUMNS || spent >= MOST_WORK {
            spent = MOST_WORK;
            return plain(text);
        }
        spent += text.len();

        // Syntax definitions are written expecting the newline `lines()` removed,
        // and `extra_newlines` is the set built for exactly that -- without it a
        // line comment never ends and colours the rest of the file.
        let owned = format!("{text}\n");
        let Ok(ops) = state.parse_line(&owned, set) else {
            // A grammar that failed is not a file the King should be denied.
            // Stop trying for the rest of it, for the reason above.
            spent = MOST_WORK;
            return plain(text);
        };

        let mut spans: Vec<CodeSpan> = Vec::new();
        let mut last = 0usize;
        let mut token = classify(&stack);

        for (offset, op) in &ops {
            // Clamped because the offsets index the string *with* the newline,
            // and a scope that opens on it would otherwise slice past the end.
            let offset = (*offset).min(text.len());
            if offset > last {
                push(&mut spans, &text[last..offset], token);
                last = offset;
            }
            let _ = stack.apply(op);
            token = classify(&stack);
        }

        if last < text.len() {
            push(&mut spans, &text[last..], token);
        }

        if spans.is_empty() {
            // An empty line still needs a span: the row is rendered from them,
            // and none at all would collapse its height.
            return plain(text);
        }
        spans
    })
}

/// One span, uncoloured -- the shape every fallback here returns.
fn plain(text: &str) -> Vec<CodeSpan> {
    vec![CodeSpan {
        text: text.to_string(),
        token: Token::Plain,
    }]
}

/// Appends a run, merging it into the previous one when they agree.
///
/// Syntect emits an operation per scope change, and most changes do not cross
/// one of our seven categories -- so without merging, a line of Rust becomes
/// dozens of adjacent spans that would all paint identically. Measured on
/// `model.rs`: merging holds it to ~2.9 spans per line against 47,593 raw
/// operations, which is the difference between a 3x and a 10x DOM.
fn push(spans: &mut Vec<CodeSpan>, text: &str, token: Token) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.token == token {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(CodeSpan {
        text: text.to_string(),
        token,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(path: &str, source: &str) -> Vec<SourceLine> {
        let all: Vec<&str> = source.lines().collect();
        lines(path, &all)
    }

    /// The load-bearing one.
    ///
    /// A note the King writes quotes the line it stands against, so spans that
    /// did not rejoin to exactly what was read would make every note against a
    /// coloured line misquote itself to the court. Checked over several
    /// languages, and including the things most likely to be mishandled: a tab,
    /// a non-ASCII character, an empty line and a trailing space.
    #[test]
    fn spans_always_rejoin_to_the_line_they_came_from() {
        let cases = [
            ("a.rs", "fn main() {\n\tlet x = \"héllo\"; // wörld\n\n}\n"),
            (
                "b.ts",
                "export const f = (x: number) => `v${x}`;\n\n// end \n",
            ),
            ("c.scss", ".a { color: $ink; }\n\n// note\n"),
            ("d.toml", "[package]\nname = \"kingdom\"\n\n"),
            ("e.py", "def f(x):\n    return {'a': 1}  # ok\n"),
            ("f.md", "# Title\n\n- one `code`\n"),
            ("g.unknownext", "nothing is known about this\n\n"),
            ("Makefile", "all:\n\techo hi\n"),
        ];

        for (path, source) in cases {
            let want: Vec<&str> = source.lines().collect();
            let got = joined(path, source);
            assert_eq!(got.len(), want.len(), "{path}: line count");
            for (line, original) in got.iter().zip(want) {
                assert_eq!(line.text(), original, "{path}: line {}", line.number);
            }
        }
    }

    /// The minified-bundle defence, which is the one guard with a real file
    /// behind it -- see the module note.
    #[test]
    fn a_line_too_wide_to_be_code_is_left_plain() {
        let wide = format!("let x = \"{}\";", "y".repeat(MOST_COLUMNS));
        let got = joined("min.js", &wide);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].spans.len(), 1, "one span, not one per token");
        assert_eq!(got[0].spans[0].token, Token::Plain);
        assert_eq!(got[0].text(), wide, "and still exactly what was read");
    }

    /// A file whose language is not known reads exactly as it did before this
    /// module existed, rather than failing or being dropped.
    #[test]
    fn an_unknown_language_is_plain_but_whole() {
        let got = joined("notes.somethingodd", "alpha\nbeta\n");

        assert_eq!(got.len(), 2);
        for line in &got {
            assert_eq!(line.spans.len(), 1);
            assert_eq!(line.spans[0].token, Token::Plain);
        }
        assert_eq!(got[1].text(), "beta");
    }

    /// The whole point: something is actually coloured, and the obvious things
    /// land in the obvious buckets.
    #[test]
    fn rust_is_actually_classified() {
        let got = joined("a.rs", "// hello\npub fn add(a: u32) -> u32 { 1 }\n");

        let comment = &got[0];
        assert_eq!(comment.spans.len(), 1, "a comment line is one run");
        assert_eq!(comment.spans[0].token, Token::Comment);

        let kinds: Vec<Token> = got[1].spans.iter().map(|s| s.token).collect();
        assert!(kinds.contains(&Token::Keyword), "pub/fn: {kinds:?}");
        assert!(kinds.contains(&Token::Function), "add: {kinds:?}");
        assert!(kinds.contains(&Token::Number), "1: {kinds:?}");
    }

    /// Adjacent runs of the same kind are one span. Without this a line is
    /// dozens of identically-painted nodes -- see `push`.
    #[test]
    fn runs_of_one_kind_are_merged() {
        let got = joined("a.rs", "// a comment with several words in it\n");
        assert_eq!(got[0].spans.len(), 1);
    }

    /// An empty line still has a span, or the row it draws would collapse.
    #[test]
    fn an_empty_line_still_has_one_span() {
        let got = joined("a.rs", "fn a() {}\n\nfn b() {}\n");
        assert_eq!(got[1].spans.len(), 1);
        assert_eq!(got[1].text(), "");
    }

    /// Comments are the case a missing newline breaks: syntect's non-newline
    /// set never closes one, and the whole file after it turns into a comment.
    #[test]
    fn a_line_comment_ends_at_its_line() {
        let got = joined("a.rs", "// comment\nlet x = 1;\n");
        let after: Vec<Token> = got[1].spans.iter().map(|s| s.token).collect();
        assert!(
            !after.iter().all(|t| *t == Token::Comment),
            "the comment leaked into the next line: {after:?}"
        );
    }
}
