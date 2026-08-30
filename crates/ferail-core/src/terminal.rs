//! Terminal launch preferences: the platform-neutral half of the
//! "Open Terminal Here" command (docs/features/CONTEXT_MENU.md).
//!
//! The GPUI app resolves the persisted settings into a [`TerminalSpec`]
//! and hands it to `platform_shell::open_terminal_with`, which turns it
//! into a concrete process launch. Everything string-shaped lives here so
//! the parsing (params → argv tokens, `{dir}` substitution) is identical
//! on every platform and unit-testable without a shell crate.

/// Placeholder in a params token that expands to the target directory.
/// Substituted per-token *after* [`split_args`], so a path with spaces
/// never re-splits.
pub const DIR_PLACEHOLDER: &str = "{dir}";

/// How the terminal is launched.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalMode {
    /// Plain launch as the current user.
    #[default]
    Standard,
    /// Elevated: UAC on Windows; a `sudo -s` root shell inside the
    /// terminal on macOS and Linux.
    Admin,
}

impl TerminalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TerminalMode::Standard => "standard",
            TerminalMode::Admin => "admin",
        }
    }
    // Infallible (unknown == Standard), so the fallible `FromStr` trait
    // doesn't fit; the name matches the house `as_str`/`from_str` pairs.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "admin" => TerminalMode::Admin,
            _ => TerminalMode::Standard,
        }
    }
}

/// A fully resolved "launch the user's terminal" request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSpec {
    /// Terminal program: an absolute path, a `.app` bundle (macOS), or a
    /// bare command name resolved via `PATH`. `None` == the platform
    /// default (Terminal.app / `wt.exe` / the Linux detection chain).
    pub program: Option<String>,
    /// Extra argv tokens, already split (see [`split_args`]). Tokens may
    /// contain [`DIR_PLACEHOLDER`]. Empty == the platform's default
    /// arguments for the chosen program.
    pub args: Vec<String>,
    pub mode: TerminalMode,
}

impl TerminalSpec {
    /// The custom program, trimmed; `None` when unset or blank.
    pub fn program(&self) -> Option<&str> {
        self.program
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn admin(&self) -> bool {
        self.mode == TerminalMode::Admin
    }

    /// Expand [`DIR_PLACEHOLDER`] in every arg token. Returns the tokens
    /// and whether any token contained the placeholder: callers that got
    /// `false` should still set the child's working directory to `dir`.
    pub fn resolved_args(&self, dir: &str) -> (Vec<String>, bool) {
        let mut had_placeholder = false;
        let args = self
            .args
            .iter()
            .map(|a| {
                if a.contains(DIR_PLACEHOLDER) {
                    had_placeholder = true;
                    a.replace(DIR_PLACEHOLDER, dir)
                } else {
                    a.clone()
                }
            })
            .collect();
        (args, had_placeholder)
    }
}

/// Split a user-typed params string into argv tokens: whitespace
/// separates, double quotes group (and may open mid-token, as in
/// `--title="my term"`). No backslash escapes: a literal `"` inside an
/// argument isn't representable, which no terminal flag needs.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut quoted = false; // token contained quotes → keep even if empty
    for ch in s.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() || quoted {
                    out.push(std::mem::take(&mut cur));
                }
                quoted = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() || quoted {
        out.push(cur);
    }
    out
}

/// The file name of a program path, lowercased, without a `.exe` suffix,
/// `/usr/bin/gnome-terminal` → `gnome-terminal`, `C:\WT\wt.exe` → `wt`.
/// Both separators are handled so a Windows-style value parses on any host.
pub fn program_basename(program: &str) -> String {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    base.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(base)
}

/// The argv tokens a terminal emulator uses to introduce a command line
/// ("run this instead of a plain shell"), keyed by program basename.
/// Used by admin mode to append `sudo -s`. `-e` is the historical X11
/// convention and the safest default for unknown emulators.
pub fn exec_prefix_for(program: &str) -> &'static [&'static str] {
    match program_basename(program).as_str() {
        // GNOME's tools dropped `-e` for a `--` separator.
        "gnome-terminal" | "kgx" | "ptyxis" => &["--"],
        // `wezterm start -- cmd`; bare `wezterm -e` is a deprecated alias.
        "wezterm" => &["start", "--"],
        // xfce4-terminal's `-e` takes ONE string; `-x` takes the rest of argv.
        "xfce4-terminal" => &["-x"],
        _ => &["-e"],
    }
}

/// The command admin mode runs inside the terminal on POSIX platforms:
/// an interactive root shell, with the password prompt in the terminal
/// itself.
pub const POSIX_ADMIN_SHELL: &[&str] = &["sudo", "-s"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_args_plain_and_quoted() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("  -d  {dir} "), vec!["-d", "{dir}"]);
        assert_eq!(
            split_args(r#"--working-directory "{dir}" -x"#),
            vec!["--working-directory", "{dir}", "-x"]
        );
        // Quote opening mid-token glues the pieces together.
        assert_eq!(
            split_args(r#"--title="my term" go"#),
            vec!["--title=my term", "go"]
        );
        // An explicitly quoted empty token survives.
        assert_eq!(split_args(r#"-e """#), vec!["-e", ""]);
    }

    #[test]
    fn resolved_args_substitutes_dir() {
        let spec = TerminalSpec {
            args: vec!["-d".into(), "{dir}".into()],
            ..Default::default()
        };
        let (args, had) = spec.resolved_args("/tmp/a b");
        assert!(had);
        assert_eq!(args, vec!["-d", "/tmp/a b"]);

        let spec = TerminalSpec::default();
        let (args, had) = spec.resolved_args("/tmp");
        assert!(!had);
        assert!(args.is_empty());
    }

    #[test]
    fn blank_program_is_none() {
        let spec = TerminalSpec {
            program: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(spec.program(), None);
    }

    #[test]
    fn exec_prefixes() {
        assert_eq!(exec_prefix_for("/usr/bin/gnome-terminal"), &["--"]);
        assert_eq!(exec_prefix_for(r"C:\tools\WezTerm.EXE"), &["start", "--"]);
        assert_eq!(exec_prefix_for("xfce4-terminal"), &["-x"]);
        assert_eq!(exec_prefix_for("konsole"), &["-e"]);
        assert_eq!(exec_prefix_for("something-new"), &["-e"]);
    }

    #[test]
    fn mode_round_trips() {
        for m in [TerminalMode::Standard, TerminalMode::Admin] {
            assert_eq!(TerminalMode::from_str(m.as_str()), m);
        }
        assert_eq!(TerminalMode::from_str("garbage"), TerminalMode::Standard);
    }
}
