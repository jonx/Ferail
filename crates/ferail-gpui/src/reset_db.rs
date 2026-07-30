//! `--reset-db <scope>` pre-event-loop CLI handler.
//!
//! Runs before any window is opened so the user can do
//! `Ferail --reset-db all` to nuke the metadata DB without us
//! re-creating it on startup. Returns `Some(exit_code)` when the
//! flag was found (caller exits with that code); `None` means the
//! flag wasn't on the command line and normal startup proceeds.

pub fn handle_reset_db_cli() -> Option<i32> {
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        if arg != "--reset-db" {
            continue;
        }
        let Some(raw) = iter.next() else {
            print_reset_db_usage();
            return Some(0);
        };
        if matches!(raw.as_str(), "help" | "--help" | "-h" | "list") {
            print_reset_db_usage();
            return Some(0);
        }
        let Some(scope) = ferail_meta::ResetScope::from_cli(&raw) else {
            eprintln!("--reset-db: unknown scope `{raw}`");
            eprintln!();
            print_reset_db_usage();
            return Some(2);
        };
        let Some(path) = ferail_meta::default_db_path() else {
            eprintln!("--reset-db: $HOME unset; nothing to reset");
            return Some(1);
        };
        if !path.exists() {
            eprintln!("--reset-db: no DB at {} (nothing to do)", path.display());
            return Some(0);
        }
        if let Err(e) = ferail_meta::ensure_parent_dir(&path) {
            eprintln!("--reset-db: mkdir failed for {}: {e}", path.display());
            return Some(1);
        }
        if matches!(scope, ferail_meta::ResetScope::All) {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("--reset-db: rm {}: {e}", path.display());
                return Some(1);
            }
            eprintln!("--reset-db: deleted {}", path.display());
            return Some(0);
        }
        let db = match ferail_meta::MetadataDb::open(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("--reset-db: open {}: {e}", path.display());
                return Some(1);
            }
        };
        match db.reset(scope) {
            Ok(()) => {
                eprintln!(
                    "--reset-db {:?}: cleared {} at {}",
                    scope,
                    scope.help_label(),
                    path.display()
                );
                return Some(0);
            }
            Err(e) => {
                eprintln!("--reset-db {:?} failed: {e}", scope);
                return Some(1);
            }
        }
    }
    None
}

fn print_reset_db_usage() {
    use ferail_meta::ResetScope;
    eprintln!("Reset parts of the metadata DB at:");
    if let Some(p) = ferail_meta::default_db_path() {
        eprintln!("  {}", p.display());
    } else {
        eprintln!("  (no DB — $HOME unset)");
    }
    eprintln!();
    eprintln!("Usage:  ferail-gpui --reset-db <scope>");
    eprintln!();
    eprintln!("Available scopes:");
    for scope in [
        ResetScope::All,
        ResetScope::Ui,
        ResetScope::Caches,
        ResetScope::AntTrail,
        ResetScope::Magic,
        ResetScope::Quarantine,
        ResetScope::Favorites,
    ] {
        let name = match scope {
            ResetScope::All => "all",
            ResetScope::Ui => "ui",
            ResetScope::Caches => "caches",
            ResetScope::AntTrail => "ant-trail",
            ResetScope::Magic => "magic",
            ResetScope::Quarantine => "quarantine",
            ResetScope::Favorites => "favorites",
        };
        eprintln!("  {:<12} {}", name, scope.help_label());
    }
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  ferail-gpui --reset-db ui          # forget window size + open tabs");
    eprintln!("  ferail-gpui --reset-db caches      # re-sniff magic + re-walk Ant Trail");
    eprintln!("  ferail-gpui --reset-db all         # nuke the DB file outright");
}
