//! Fill in syntax highlighting for languages that gpui-component ships
//! with a *grammar* but an empty *highlights query*, so they'd render
//! plain in the preview pane (docs/features/PREVIEW.md). In the pinned
//! rev that's C#, C, C++, Bash, Swift, and CMake.
//!
//! We reuse the `tree_sitter::Language` already compiled into the
//! highlighter registry and only supply a vendored query (the grammar
//! crate's own `queries/highlights.scm`, copied under
//! `src/syntax_queries/`). That means **no grammar-crate dependencies
//! and no tree-sitter version coupling**: the grammar is whatever
//! gpui-component already built. Capture names that aren't in the
//! registry's vocabulary degrade through its `.`-prefix fallback
//! (e.g. `type.builtin` → `type`), and a query that somehow fails to
//! compile falls back to plain text (the highlighter logs and uses
//! `text`), never panics.

use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};

/// `(registry language name, vendored highlights query)`. The name
/// must match the registry's (`csharp`, not `cs`: aliases resolve to
/// these canonical names).
const QUERIES: &[(&str, &str)] = &[
    ("csharp", include_str!("syntax_queries/csharp.scm")),
    ("c", include_str!("syntax_queries/c.scm")),
    ("cpp", include_str!("syntax_queries/cpp.scm")),
    ("bash", include_str!("syntax_queries/bash.scm")),
    ("swift", include_str!("syntax_queries/swift.scm")),
    ("cmake", include_str!("syntax_queries/cmake.scm")),
];

/// Register the vendored queries. Run once at startup, before any
/// preview renders. Idempotent (re-registering overwrites). A name the
/// registry doesn't know, e.g. the grammar feature was disabled, is
/// skipped silently.
pub fn register_extra_languages() {
    let registry = LanguageRegistry::singleton();
    for (name, query) in QUERIES {
        let Some(existing) = registry.language(name) else {
            continue;
        };
        // `language` is `None` for grammarless (plain-text) registry
        // entries; our queries are useless without the grammar.
        let Some(language) = existing.language.clone() else {
            continue;
        };
        let config = LanguageConfig::new(
            *name,
            language,
            existing.injection_languages.clone(),
            query,
            "",
            "",
        );
        registry.register(name, &config);
    }
}
