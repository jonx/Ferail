//! CLDR plural categories for integer counts, for the languages Ferail is
//! likely to be translated into. Integer rules only — UI counts are whole
//! numbers. Unknown languages get the English rule (`one` for exactly 1).
//!
//! Extend [`category`] when a new language needs it; the rest of the
//! system (packs, `trn!`, the translation instructions) is
//! category-agnostic.

/// The six CLDR plural categories. Pack files use the lower-case names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::Zero,
        Category::One,
        Category::Two,
        Category::Few,
        Category::Many,
        Category::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Zero => "zero",
            Category::One => "one",
            Category::Two => "two",
            Category::Few => "few",
            Category::Many => "many",
            Category::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "zero" => Category::Zero,
            "one" => Category::One,
            "two" => Category::Two,
            "few" => Category::Few,
            "many" => Category::Many,
            "other" => Category::Other,
            _ => return None,
        })
    }
}

/// Which plural form `n` takes in language `lang` (a bare subtag: `"fr"`,
/// not `"fr-FR"` — use [`super::language_subtag`]).
pub fn category(lang: &str, n: u64) -> Category {
    use Category::*;
    let n10 = n % 10;
    let n100 = n % 100;
    match lang {
        // No number agreement.
        "ja" | "zh" | "ko" | "vi" | "th" | "id" | "ms" | "lo" | "km" | "my" | "yue" => Other,
        // 0 and 1 are singular.
        "fr" | "pt" | "hy" | "kab" => {
            if n <= 1 {
                One
            } else {
                Other
            }
        }
        "ru" | "uk" | "be" => {
            if n10 == 1 && n100 != 11 {
                One
            } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
                Few
            } else {
                Many
            }
        }
        "pl" => {
            if n == 1 {
                One
            } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
                Few
            } else {
                Many
            }
        }
        "cs" | "sk" => {
            if n == 1 {
                One
            } else if (2..=4).contains(&n) {
                Few
            } else {
                Other
            }
        }
        "hr" | "sr" | "bs" => {
            if n10 == 1 && n100 != 11 {
                One
            } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
                Few
            } else {
                Other
            }
        }
        "lt" => {
            if n10 == 1 && !(11..=19).contains(&n100) {
                One
            } else if (2..=9).contains(&n10) && !(11..=19).contains(&n100) {
                Few
            } else {
                Other
            }
        }
        "lv" => {
            if n10 == 0 || (11..=19).contains(&n100) {
                Zero
            } else if n10 == 1 && n100 != 11 {
                One
            } else {
                Other
            }
        }
        "ro" => {
            if n == 1 {
                One
            } else if n == 0 || ((1..=19).contains(&n100) && n != 1) {
                Few
            } else {
                Other
            }
        }
        "sl" => match n100 {
            1 => One,
            2 => Two,
            3 | 4 => Few,
            _ => Other,
        },
        "ar" => {
            if n == 0 {
                Zero
            } else if n == 1 {
                One
            } else if n == 2 {
                Two
            } else if (3..=10).contains(&n100) {
                Few
            } else if (11..=99).contains(&n100) {
                Many
            } else {
                Other
            }
        }
        "he" | "iw" => {
            if n == 1 {
                One
            } else if n == 2 {
                Two
            } else {
                Other
            }
        }
        "ga" => match n {
            1 => One,
            2 => Two,
            3..=6 => Few,
            7..=10 => Many,
            _ => Other,
        },
        "cy" => match n {
            0 => Zero,
            1 => One,
            2 => Two,
            3 => Few,
            6 => Many,
            _ => Other,
        },
        // en, de, nl, es, it, sv, da, nb, nn, fi, et, el, hu, tr, bg, ca,
        // gl, eu, af, sq, ka, … and anything unknown.
        _ => {
            if n == 1 {
                One
            } else {
                Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Category::*, category};

    #[test]
    fn english_rule() {
        assert_eq!(category("en", 0), Other);
        assert_eq!(category("en", 1), One);
        assert_eq!(category("en", 2), Other);
        assert_eq!(category("xx", 1), One);
    }

    #[test]
    fn french_zero_is_singular() {
        assert_eq!(category("fr", 0), One);
        assert_eq!(category("fr", 1), One);
        assert_eq!(category("fr", 2), Other);
    }

    #[test]
    fn russian_forms() {
        assert_eq!(category("ru", 1), One);
        assert_eq!(category("ru", 21), One);
        assert_eq!(category("ru", 11), Many);
        assert_eq!(category("ru", 3), Few);
        assert_eq!(category("ru", 13), Many);
        assert_eq!(category("ru", 5), Many);
        assert_eq!(category("ru", 0), Many);
    }

    #[test]
    fn polish_forms() {
        assert_eq!(category("pl", 1), One);
        assert_eq!(category("pl", 21), Many);
        assert_eq!(category("pl", 22), Few);
        assert_eq!(category("pl", 12), Many);
    }

    #[test]
    fn arabic_forms() {
        assert_eq!(category("ar", 0), Zero);
        assert_eq!(category("ar", 1), One);
        assert_eq!(category("ar", 2), Two);
        assert_eq!(category("ar", 5), Few);
        assert_eq!(category("ar", 15), Many);
        assert_eq!(category("ar", 100), Other);
    }

    #[test]
    fn japanese_has_no_agreement() {
        assert_eq!(category("ja", 1), Other);
    }
}
