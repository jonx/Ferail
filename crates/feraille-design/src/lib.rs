//! Design tokens. Single source of truth for colors, spacing, typography.
//! See `specs/controls/01-design-tokens.md`. No raw color literals or pixel
//! values exist outside this crate — they would be a spec violation.
//!
//! Iteration 2: Zed-aligned. Two foreground tiers (no tertiary), single
//! accent (no hover/pressed variants — Zed doesn't tint hover with accent),
//! sharp corners by default (Zed's `Corners::default()` is zero), subtle
//! background-layer deltas (~6% in dark, ~3% in light).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xFF }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Pack to 0xAARRGGBB, the renderer's internal format.
    pub fn pack_argb(self) -> u32 {
        ((self.a as u32) << 24)
            | ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontWeight {
    Regular,
    Medium,
    SemiBold,
}

#[derive(Clone, Debug)]
pub struct Tokens {
    pub theme: Theme,
    pub bg: BackgroundTokens,
    pub fg: ForegroundTokens,
    pub accent: AccentTokens,
    pub border: BorderTokens,
    pub status: StatusTokens,
    pub space: SpacingTokens,
    pub radius: RadiusTokens,
    pub text: TextTokens,
    pub hit: HitTokens,
    pub magic: MagicTokens,
}

#[derive(Clone, Debug)]
pub struct BackgroundTokens {
    pub base: Color,
    pub layer1: Color,
    pub layer2: Color,
    pub layer3: Color,
}

/// Two real tiers: `primary` (body) and `secondary` (metadata, second
/// columns). `disabled` and `on_accent` are special slots, not a tier.
#[derive(Clone, Debug)]
pub struct ForegroundTokens {
    pub primary: Color,
    pub secondary: Color,
    pub disabled: Color,
    pub on_accent: Color,
}

/// One accent. No hover/pressed variants — Zed reserves accent for
/// selection/focus only. Hover uses `bg.layer3`.
#[derive(Clone, Debug)]
pub struct AccentTokens {
    pub fill: Color,
    pub subtle: Color,
    pub subtle_inactive: Color,
}

#[derive(Clone, Debug)]
pub struct BorderTokens {
    pub subtle: Color,
    pub default: Color,
    pub focus: Color,
}

#[derive(Clone, Debug)]
pub struct StatusTokens {
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
}

#[derive(Clone, Copy, Debug)]
pub struct SpacingTokens {
    pub xxs: f32,
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub xxl: f32,
}

/// Sharp by default. `popover` exists for a future rounded surface
/// (context menu / tooltip); currently unused.
#[derive(Clone, Copy, Debug)]
pub struct RadiusTokens {
    pub none: f32,
    pub popover: f32,
    pub full: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct TextTokens {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct HitTokens {
    pub min: f32,
    pub row: f32,
    pub button: f32,
    pub input: f32,
}

/// Categorical icon-tint palette for the Magic / file-type column.
/// Saturated enough to read in both light and dark mode, so light()
/// and dark() share the same values.
#[derive(Clone, Copy, Debug)]
pub struct MagicTokens {
    /// Source code, scripts.
    pub code: Color,
    /// Raster + vector images.
    pub image: Color,
    /// Audio + video.
    pub media: Color,
    /// Compressed archives.
    pub archive: Color,
    /// Structured data / config.
    pub data: Color,
    /// Documents (markdown, pdf, doc, txt).
    pub doc: Color,
}

impl Tokens {
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::light(),
            Theme::Dark => Self::dark(),
        }
    }

    pub fn light() -> Self {
        Self {
            theme: Theme::Light,
            bg: BackgroundTokens {
                base: Color::rgb(0xFA, 0xFA, 0xFA),
                layer1: Color::rgb(0xFF, 0xFF, 0xFF),
                layer2: Color::rgb(0xF4, 0xF4, 0xF4),
                layer3: Color::rgb(0xEC, 0xEC, 0xEC),
            },
            fg: ForegroundTokens {
                primary: Color::rgb(0x1A, 0x1A, 0x1A),
                secondary: Color::rgb(0x6F, 0x6F, 0x6F),
                disabled: Color::rgb(0xB0, 0xB0, 0xB0),
                on_accent: Color::rgb(0xFF, 0xFF, 0xFF),
            },
            accent: AccentTokens {
                fill: Color::rgb(0x2A, 0x63, 0xD9),
                // ~18% accent on light surfaces, perceptually subtle but distinct.
                subtle: Color::rgba(0x2A, 0x63, 0xD9, 46),
                // Neutral gray for unfocused selection — no accent leak.
                subtle_inactive: Color::rgba(0x6F, 0x6F, 0x6F, 28),
            },
            border: BorderTokens {
                subtle: Color::rgb(0xE5, 0xE5, 0xE5),
                default: Color::rgb(0xD1, 0xD1, 0xD1),
                focus: Color::rgb(0x2A, 0x63, 0xD9),
            },
            status: StatusTokens {
                success: Color::rgb(0x10, 0x7C, 0x10),
                warning: Color::rgb(0x9D, 0x5D, 0x00),
                danger: Color::rgb(0xC4, 0x2B, 0x1C),
            },
            space: SpacingTokens {
                xxs: 2.0, xs: 4.0, sm: 8.0, md: 12.0,
                lg: 16.0, xl: 24.0, xxl: 32.0,
            },
            radius: RadiusTokens { none: 0.0, popover: 6.0, full: 9999.0 },
            text: TextTokens { xs: 11.0, sm: 12.0, md: 13.0, lg: 15.0, xl: 18.0 },
            hit: HitTokens { min: 24.0, row: 28.0, button: 32.0, input: 32.0 },
            magic: MagicTokens {
                code: Color::rgb(0xA0, 0x6B, 0xD9),
                image: Color::rgb(0x4F, 0xA8, 0x6E),
                media: Color::rgb(0xCC, 0x5B, 0x9C),
                archive: Color::rgb(0xC8, 0x83, 0x44),
                data: Color::rgb(0x47, 0x9C, 0xB5),
                doc: Color::rgb(0x4A, 0x80, 0xC0),
            },
        }
    }

    pub fn dark() -> Self {
        let mut t = Self::light();
        t.theme = Theme::Dark;
        t.bg = BackgroundTokens {
            base: Color::rgb(0x1B, 0x1B, 0x1B),
            layer1: Color::rgb(0x22, 0x22, 0x22),
            layer2: Color::rgb(0x26, 0x26, 0x26),
            layer3: Color::rgb(0x2D, 0x2D, 0x2D),
        };
        t.fg = ForegroundTokens {
            primary: Color::rgb(0xF5, 0xF5, 0xF5),
            secondary: Color::rgb(0x99, 0x99, 0x99),
            disabled: Color::rgb(0x5A, 0x5A, 0x5A),
            on_accent: Color::rgb(0xFF, 0xFF, 0xFF),
        };
        t.accent = AccentTokens {
            fill: Color::rgb(0x24, 0x57, 0xCA),
            subtle: Color::rgba(0x24, 0x57, 0xCA, 80),
            subtle_inactive: Color::rgba(0x99, 0x99, 0x99, 36),
        };
        t.border = BorderTokens {
            subtle: Color::rgb(0x2D, 0x2D, 0x2D),
            default: Color::rgb(0x3A, 0x3A, 0x3A),
            focus: Color::rgb(0x24, 0x57, 0xCA),
        };
        t
    }
}
