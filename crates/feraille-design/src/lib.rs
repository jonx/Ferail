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
    pub icon: IconTokens,
    pub layout: LayoutTokens,
    pub magic: MagicTokens,
    /// User UI scale applied to all numeric dimensions on construction
    /// (text, space, hit, icon, layout). Borders and radii are not
    /// scaled — they are tied to pixel-level crispness.
    /// 1.0 = baseline; clamp to [0.6, 2.5] when setting.
    pub ui_scale: f32,
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

/// Sharp by default for explorer chrome. The `sm/md/lg` triplet
/// is the semantic three-step from the design brief — `sm` for
/// controls (inputs, segments, toggles), `md` for cards / groups,
/// `lg` for modal windows and large surfaces. `popover` is kept
/// for context menus and tooltips where Apple's convention is a
/// slightly different curve.
#[derive(Clone, Copy, Debug)]
pub struct RadiusTokens {
    pub none: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub popover: f32,
    pub full: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct TextTokens {
    pub xxs: f32,
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

/// A named tier in the canonical type scale. Every text size in the app
/// resolves to one of these — the single source of truth for typography.
/// Render code maps a tier to logical px via [`TextTokens::get`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextSize {
    /// Micro labels / dense overlays.
    Xxs,
    /// Body and metadata — the workhorse tier.
    Xs,
    /// Slightly emphasized labels and rows.
    Sm,
    /// Default-weight body for roomier surfaces.
    Md,
    /// Section headers.
    Lg,
    /// Hero / title text.
    Xl,
}

impl TextTokens {
    /// The canonical type scale, in logical px at `ui_scale = 1.0`
    /// (Zed-aligned, denser than browser defaults). This is the single
    /// source of truth for text sizing. `feraille-gpui`'s `TextScale`
    /// trait reads these and applies them rem-relative, so UI zoom (the
    /// window rem size) scales every tier together. Retune the whole app
    /// by editing these six numbers.
    pub const BASE: Self = Self {
        xxs: 10.0,
        xs: 11.0,
        sm: 12.0,
        md: 13.0,
        lg: 15.0,
        xl: 18.0,
    };

    /// Logical px for `size` in this token set.
    pub const fn get(&self, size: TextSize) -> f32 {
        match size {
            TextSize::Xxs => self.xxs,
            TextSize::Xs => self.xs,
            TextSize::Sm => self.sm,
            TextSize::Md => self.md,
            TextSize::Lg => self.lg,
            TextSize::Xl => self.xl,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HitTokens {
    pub min: f32,
    pub row: f32,
    pub button: f32,
    pub input: f32,
}

/// Icon dimensions for paint-time sizing. NSWorkspace returns icons at
/// the physical pixel resolution requested; controls pick a logical
/// size from this struct for hit-rect / cell layout.
#[derive(Clone, Copy, Debug)]
pub struct IconTokens {
    /// Tree rows, sidebar rows, chevron-area glyphs.
    pub sm: f32,
    /// List rows, tab close button, status icons.
    pub md: f32,
    /// Toolbar buttons, larger affordances.
    pub lg: f32,
    /// Preview pane / properties pane hero icon.
    pub xl: f32,
}

/// Layout dimensions used by the App-level frame and individual
/// controls. Heights are row-major; widths are pane defaults / clamps.
#[derive(Clone, Copy, Debug)]
pub struct LayoutTokens {
    /// FileTree row height (denser than `hit.row`).
    pub tree_row: f32,
    /// FileTree section-header row height.
    pub tree_section_header: f32,
    /// FileTree per-depth indent, applied left of the chevron.
    pub tree_indent: f32,
    /// FileTree chevron-column width.
    pub tree_chevron_w: f32,
    /// Tabstrip total height.
    pub tabstrip: f32,
    /// Breadcrumb total height.
    pub breadcrumb: f32,
    /// Status-bar total height.
    pub status_bar: f32,
    /// Sidebar default / min / max widths (splitter clamps).
    pub sidebar_default: f32,
    pub sidebar_min: f32,
    pub sidebar_max: f32,
    /// Preview pane default / min / max widths.
    pub preview_default: f32,
    pub preview_min: f32,
    pub preview_max: f32,
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

/// Clamp range for `Tokens::scaled`. Values below 0.6 produce
/// unreadable text; above 2.5 the layout overflows reasonable
/// windows.
pub const UI_SCALE_MIN: f32 = 0.6;
pub const UI_SCALE_MAX: f32 = 2.5;

impl Tokens {
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::light(),
            Theme::Dark => Self::dark(),
        }
    }

    /// Build a scaled copy of this token set. Multiplies every numeric
    /// dimension that contributes to UI density (text, spacing, hit
    /// rects, icons, layout heights / widths) by `scale`. Borders and
    /// radii are not scaled — they're tied to crisp pixel rendering.
    /// `scale` is clamped to `[UI_SCALE_MIN, UI_SCALE_MAX]` so a stray
    /// keystroke can't break the layout.
    ///
    /// Idempotent in the sense that calling `scaled(1.0)` is a no-op.
    /// Re-scaling an already-scaled `Tokens` compounds — callers should
    /// always start from a freshly-built `for_theme(...)`.
    pub fn scaled(mut self, scale: f32) -> Self {
        let s = scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX);
        self.ui_scale = s;
        if (s - 1.0).abs() < f32::EPSILON {
            return self;
        }

        self.space.xxs *= s;
        self.space.xs *= s;
        self.space.sm *= s;
        self.space.md *= s;
        self.space.lg *= s;
        self.space.xl *= s;
        self.space.xxl *= s;

        self.text.xxs *= s;
        self.text.xs *= s;
        self.text.sm *= s;
        self.text.md *= s;
        self.text.lg *= s;
        self.text.xl *= s;

        self.hit.min *= s;
        self.hit.row *= s;
        self.hit.button *= s;
        self.hit.input *= s;

        self.icon.sm *= s;
        self.icon.md *= s;
        self.icon.lg *= s;
        self.icon.xl *= s;

        self.layout.tree_row *= s;
        self.layout.tree_section_header *= s;
        self.layout.tabstrip *= s;
        self.layout.breadcrumb *= s;
        self.layout.status_bar *= s;
        self.layout.sidebar_default *= s;
        self.layout.sidebar_min *= s;
        self.layout.sidebar_max *= s;
        self.layout.preview_default *= s;
        self.layout.preview_min *= s;
        self.layout.preview_max *= s;

        self
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
            radius: RadiusTokens {
                none: 0.0,
                sm: 4.0,
                md: 8.0,
                lg: 12.0,
                popover: 6.0,
                full: 9999.0,
            },
            text: TextTokens::BASE,
            hit: HitTokens { min: 24.0, row: 28.0, button: 32.0, input: 32.0 },
            icon: IconTokens { sm: 14.0, md: 16.0, lg: 20.0, xl: 32.0 },
            layout: LayoutTokens {
                tree_row: 24.0,
                tree_section_header: 26.0,
                tree_indent: 16.0,
                tree_chevron_w: 14.0,
                tabstrip: 32.0,
                breadcrumb: 32.0,
                status_bar: 24.0,
                sidebar_default: 220.0,
                sidebar_min: 160.0,
                sidebar_max: 480.0,
                preview_default: 320.0,
                preview_min: 220.0,
                preview_max: 600.0,
            },
            ui_scale: 1.0,
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
