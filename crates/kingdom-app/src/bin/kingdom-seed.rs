//! `kingdom-seed` -- materialises a proving ground on disk.
//!
//! A separate binary rather than a flag on the server, because seeding is a
//! one-shot job with an exit code: a CI step or a `make` target can depend on
//! it, and it needs no Axum runtime.
//!
//! ```text
//! cargo run -p kingdom-app --bin kingdom-seed -- kingdom-mirror
//! cargo run -p kingdom-app --bin kingdom-seed -- --list
//! ```

#[cfg(feature = "ssr")]
fn main() -> std::process::ExitCode {
    use kingdom_app::mock;
    use kingdom_core::mockdata;

    let mut fixture_name: Option<String> = None;
    let mut into: Option<std::path::PathBuf> = None;
    let mut force = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" | "-l" => {
                list();
                return std::process::ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                usage();
                return std::process::ExitCode::SUCCESS;
            }
            "--force" | "-f" => force = true,
            "--into" => match args.next() {
                Some(dir) => into = Some(std::path::PathBuf::from(dir)),
                None => {
                    eprintln!("--into needs a directory");
                    return std::process::ExitCode::FAILURE;
                }
            },
            other if other.starts_with('-') => {
                eprintln!("Unknown option: {other}");
                usage();
                return std::process::ExitCode::FAILURE;
            }
            other => fixture_name = Some(other.to_string()),
        }
    }

    let name = fixture_name.unwrap_or_else(|| mockdata::DEFAULT_FIXTURE.to_string());
    let Some(spec) = mockdata::fixture(&name) else {
        eprintln!("No such realm: {name}\n");
        list();
        return std::process::ExitCode::FAILURE;
    };

    let root = into.unwrap_or_else(|| mock::fixture_path(&name));

    // Without --force, an existing proving ground is left alone. Re-seeding is
    // destructive even when it is safe, so it should be asked for.
    if !force && mock::is_proving_ground(&root) {
        println!(
            "'{name}' already stands at {}.\nPass --force to raze and rebuild it.",
            root.display()
        );
        return std::process::ExitCode::SUCCESS;
    }

    match mock::seed(&spec, &root) {
        Ok(report) => {
            println!("{report}");
            match root.canonicalize() {
                Ok(abs) => println!("\n  Open this in Kingdom IDE:\n    {}", abs.display()),
                Err(_) => println!("\n  Open this in Kingdom IDE:\n    {}", root.display()),
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Could not seed '{name}': {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "ssr")]
fn list() {
    println!("Realms:\n");
    for fixture in kingdom_core::mockdata::fixtures() {
        let marker = if fixture.name == kingdom_core::mockdata::DEFAULT_FIXTURE {
            " (default)"
        } else {
            ""
        };
        println!("  {:<16}{}{marker}", fixture.name, fixture.blurb);
    }
    println!(
        "\nDefined in crates/kingdom-core/src/mockdata/fixtures.rs -- edit that to change them."
    );
}

#[cfg(feature = "ssr")]
fn usage() {
    println!(
        "Raise a proving ground: a synthetic dev folder, safe to work against.\n\n\
         USAGE:\n  kingdom-seed [REALM] [--into DIR] [--force]\n  kingdom-seed --list\n\n\
         OPTIONS:\n\
         \x20 --into DIR   Where to seed it. Defaults to .kingdom/realms/REALM\n\
         \x20 --force      Raze and rebuild a realm that already stands\n\
         \x20 --list       Show every realm\n"
    );
}

#[cfg(not(feature = "ssr"))]
fn main() {
    // Seeding touches the filesystem, so it exists only on the native target.
}
