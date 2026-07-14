//! Magic types: enum of detected file types + structured metadata
//! struct + the ` · `-joined description formatter.
//!
//! Ported from bfe-explorer (`crates/ferail-ui/src/magic/types.rs`) with
//! two adaptations:
//!
//! - `MagicType::display_name()` matches feraille's existing label
//!   strings (`"PNG image"`, `"ZIP archive"`, `"PE / DOS executable"`)
//!   so the Format column doesn't visibly change. Description carries
//!   the new richer info.
//! - We drop bfe's `ZipLayout::SingleRootFolder` and `file_count` fields
//!   because both require a central-directory scan at EOF; that's a
//!   second I/O which we're not doing in this phase.

#![allow(dead_code)]

/// Content-based file type detected by reading the first 4 KB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MagicType {
    // Documents
    Pdf,
    // Office (ZIP-based)
    DocWord,
    DocWordMacro,
    DocExcel,
    DocExcelMacro,
    DocPowerPoint,
    DocPowerPointMacro,
    // Archives
    Zip,
    ZipEncrypted,
    Rar,
    SevenZip,
    Tar,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
    // App packages (ZIP-based)
    AppJar,
    AppApk,
    // Images
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
    Ico,
    Tiff,
    Heic,
    Svg,
    // Video
    Mp4,
    Mov,
    Avi,
    Mkv,
    Webm,
    // Audio
    Mp3,
    Wav,
    Flac,
    Ogg,
    Aiff,
    M4a,
    /// Advanced Systems Format — the container for WMA (audio) and WMV
    /// (video). `has_audio`/`has_video` distinguish them.
    Asf,
    // Data / Text containers
    Json,
    Xml,
    Html,
    Sqlite,
    // Executables — Windows
    ExeWindows,
    DllWindows,
    ExeWindowsNet,
    // Executables — Linux
    ExeLinux,
    SoLinux,
    // Executables — macOS
    ExeMac,
    DylibMac,
    // Universal binary (Mach-O fat / Java class — disambiguated only
    // by where they're typically found).
    MachOFat,
    // Scripts (shebang-detected)
    ScriptBash,
    ScriptPython,
    ScriptPerl,
    ScriptRuby,
    ScriptNode,
    ScriptOther,
    // Text subtypes
    TextIni,
    TextReg,
    TextPlain,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    // Windows shortcuts
    Lnk,
    Url,
    // Fonts
    OpenType,
    TrueType,
    // Generic
    Folder,
    #[default]
    Unknown,
    Binary,
}

impl MagicType {
    /// Label used by the Format column. Matches the strings the previous
    /// flat-table detector returned, so the column doesn't visibly change
    /// when this code lands.
    pub fn display_name(&self) -> &'static str {
        match self {
            // Documents
            MagicType::Pdf => "PDF document",

            // Office — extension is more familiar to users than
            // "Document (Word)" style. Keep concise.
            MagicType::DocWord | MagicType::DocWordMacro => "Word document",
            MagicType::DocExcel | MagicType::DocExcelMacro => "Excel spreadsheet",
            MagicType::DocPowerPoint | MagicType::DocPowerPointMacro => "PowerPoint presentation",

            // Archives — match the historical flat-table labels exactly
            // so existing format_label tests keep passing.
            MagicType::Zip => "ZIP archive",
            MagicType::ZipEncrypted => "ZIP archive",
            MagicType::Rar => "RAR archive",
            MagicType::SevenZip => "7z archive",
            MagicType::Tar => "TAR archive",
            MagicType::Gzip => "Gzip archive",
            MagicType::Xz => "XZ archive",
            MagicType::Bzip2 => "Bzip2 archive",
            MagicType::Zstd => "Zstandard archive",

            MagicType::AppJar => "Java JAR",
            MagicType::AppApk => "Android APK",

            // Images
            MagicType::Jpeg => "JPEG image",
            MagicType::Png => "PNG image",
            MagicType::Gif => "GIF image",
            MagicType::Bmp => "BMP image",
            MagicType::Webp => "WebP image",
            MagicType::Ico => "Icon file",
            MagicType::Tiff => "TIFF image",
            MagicType::Heic => "HEIC image",
            MagicType::Svg => "XML / SVG",

            // Video
            MagicType::Mp4 => "MP4 video",
            MagicType::Mov => "QuickTime movie",
            MagicType::Avi => "AVI video",
            MagicType::Mkv => "Matroska / WebM",
            MagicType::Webm => "Matroska / WebM",

            // Audio
            MagicType::Mp3 => "MP3 audio",
            MagicType::Wav => "WAV audio",
            MagicType::Flac => "FLAC audio",
            MagicType::Ogg => "Ogg audio",
            MagicType::Aiff => "AIFF audio",
            MagicType::M4a => "M4A audio",
            MagicType::Asf => "Windows Media",

            // Data
            MagicType::Json => "JSON",
            MagicType::Xml => "XML",
            MagicType::Html => "HTML document",
            MagicType::Sqlite => "SQLite database",

            // Executables — keep "executable" in the string so
            // icons::classify_file's substring matcher still resolves
            // to FileTypeTint::Executable.
            MagicType::ExeWindows => "PE / DOS executable",
            MagicType::DllWindows => "PE / DOS executable",
            MagicType::ExeWindowsNet => "PE / DOS executable",
            MagicType::ExeLinux => "ELF executable",
            MagicType::SoLinux => "ELF executable",
            MagicType::ExeMac => "Mach-O executable",
            MagicType::DylibMac => "Mach-O dylib",
            MagicType::MachOFat => "Mach-O fat / Java class",

            // Scripts — keep "script" in the string for the
            // classifier substring matcher.
            MagicType::ScriptBash => "Shell script",
            MagicType::ScriptPython => "Python script",
            MagicType::ScriptPerl => "Perl script",
            MagicType::ScriptRuby => "Ruby script",
            MagicType::ScriptNode => "Node.js script",
            MagicType::ScriptOther => "Script",

            // Text
            MagicType::TextIni => "INI",
            MagicType::TextReg => "Windows Registry",
            MagicType::TextPlain => "Plain text",
            MagicType::Utf8Bom => "UTF-8 text",
            MagicType::Utf16Le => "UTF-16 LE text",
            MagicType::Utf16Be => "UTF-16 BE text",

            // Windows
            MagicType::Lnk => "Windows shortcut",
            MagicType::Url => "Internet shortcut",

            // Fonts
            MagicType::OpenType => "OpenType font",
            MagicType::TrueType => "TrueType font",

            MagicType::Folder => "Folder",
            MagicType::Binary => "Binary",
            MagicType::Unknown => "",
        }
    }
}

/// CPU architecture from executable headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuArch {
    #[default]
    Unknown,
    X86,
    X64,
    Arm,
    Arm64,
    Riscv,
    PowerPc,
    Mips,
}

impl CpuArch {
    pub fn as_str(&self) -> &'static str {
        match self {
            CpuArch::Unknown => "",
            CpuArch::X86 => "x86",
            CpuArch::X64 => "x86-64",
            CpuArch::Arm => "ARM",
            CpuArch::Arm64 => "ARM64",
            CpuArch::Riscv => "RISC-V",
            CpuArch::PowerPc => "PowerPC",
            CpuArch::Mips => "MIPS",
        }
    }
}

/// Operating-system ABI recorded in an ELF header's `e_ident[EI_OSABI]`
/// byte. Most GNU/Linux toolchains leave this `0` (System V / "none"),
/// so a *named* OS here is a deliberate marker worth surfacing — AROS,
/// for instance, stamps `ELFOSABI_AROS` (15) on every binary it builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElfOs {
    /// System V / "none" (0) or any ABI we don't name — rendered as no tag.
    #[default]
    Unknown,
    Linux,
    FreeBsd,
    NetBsd,
    OpenBsd,
    Solaris,
    Aros,
}

impl ElfOs {
    /// Display name for the description column, or `""` for the generic
    /// System V ABI (so ordinary Linux binaries gain no OS suffix).
    pub fn as_str(&self) -> &'static str {
        match self {
            ElfOs::Unknown => "",
            ElfOs::Linux => "Linux",
            ElfOs::FreeBsd => "FreeBSD",
            ElfOs::NetBsd => "NetBSD",
            ElfOs::OpenBsd => "OpenBSD",
            ElfOs::Solaris => "Solaris",
            ElfOs::Aros => "AROS",
        }
    }
}

/// PE subsystem (Windows executable kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeSubsystem {
    #[default]
    Unknown,
    Console,
    Gui,
    Native,
}

impl PeSubsystem {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeSubsystem::Unknown => "",
            PeSubsystem::Console => "console",
            PeSubsystem::Gui => "GUI",
            PeSubsystem::Native => "native",
        }
    }
}

/// Structured facts extracted from the first 4 KB of the file. All
/// fields are optional — populate only what was cheap to read.
///
/// The `description()` method turns this into the ` · `-joined string
/// rendered in the Description column.
#[derive(Debug, Clone, Default)]
pub struct MagicInfo {
    pub magic_type: MagicType,

    // Executable details
    pub is_64bit: Option<bool>,
    pub arch: CpuArch,
    pub subsystem: PeSubsystem,
    pub is_dotnet: bool,
    /// Operating-system ABI from an ELF header (`EI_OSABI`). Default
    /// `ElfOs::Unknown` for the generic System V ABI and all non-ELF types.
    pub os: ElfOs,
    /// ELF `e_type == ET_REL`: a relocatable object, not a runnable image.
    /// AROS ships its libraries (e.g. `exec.library`) as relocatables.
    pub is_relocatable: bool,

    // Office / archive
    pub has_macros: bool,
    pub is_encrypted: bool,

    // ZIP central-directory facts (filled from the tail-4-KB pass for
    // ZIP-based types).
    /// Total number of entries reported by the End-of-Central-Directory
    /// record. `None` when the CD couldn't be parsed.
    pub file_count: Option<u32>,
    /// Single-root-folder name when every CD entry sits under the same
    /// top-level directory. `None` means we either didn't walk the CD
    /// or the archive is flat / multi-rooted.
    pub zip_root: Option<String>,

    // Image
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_alpha: bool,

    // Audio
    pub channels: Option<u8>,
    pub sample_rate: Option<u32>,
    pub bitrate_kbps: Option<u16>,
    pub duration_secs: Option<u32>,

    // Video
    pub has_video: bool,
    pub has_audio: bool,

    // Script
    pub interpreter: Option<&'static str>,
}

impl MagicInfo {
    pub fn new(magic_type: MagicType) -> Self {
        Self {
            magic_type,
            ..Default::default()
        }
    }

    /// Render a stable ` · `-joined fact string for the Description
    /// column. Empty string for types with nothing extra to say.
    pub fn description(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        match self.magic_type {
            // Windows executables
            MagicType::ExeWindows | MagicType::DllWindows | MagicType::ExeWindowsNet => {
                parts.push("Windows PE".into());
                if let Some(is_64) = self.is_64bit {
                    parts.push(if is_64 { "64-bit".into() } else { "32-bit".into() });
                }
                let arch = self.arch.as_str();
                if !arch.is_empty() {
                    parts.push(arch.into());
                }
                let sub = self.subsystem.as_str();
                if !sub.is_empty() {
                    parts.push(sub.into());
                }
                if matches!(self.magic_type, MagicType::DllWindows) {
                    parts.push("library".into());
                }
                if self.is_dotnet {
                    parts.push(".NET".into());
                }
            }
            MagicType::ExeLinux | MagicType::SoLinux => {
                parts.push("ELF".into());
                if let Some(is_64) = self.is_64bit {
                    parts.push(if is_64 { "64-bit".into() } else { "32-bit".into() });
                }
                let kind = if self.is_relocatable {
                    "relocatable"
                } else if matches!(self.magic_type, MagicType::SoLinux) {
                    "shared object"
                } else {
                    "executable"
                };
                parts.push(kind.into());
                let arch = self.arch.as_str();
                if !arch.is_empty() {
                    parts.push(arch.into());
                }
                let os = self.os.as_str();
                if !os.is_empty() {
                    parts.push(os.into());
                }
            }
            MagicType::ExeMac | MagicType::DylibMac => {
                parts.push("Mach-O".into());
                if let Some(is_64) = self.is_64bit {
                    parts.push(if is_64 { "64-bit".into() } else { "32-bit".into() });
                }
                parts.push(
                    if matches!(self.magic_type, MagicType::DylibMac) {
                        "dylib".into()
                    } else {
                        "executable".into()
                    },
                );
                let arch = self.arch.as_str();
                if !arch.is_empty() {
                    parts.push(arch.into());
                }
            }

            // Office documents
            MagicType::DocWord | MagicType::DocWordMacro => {
                parts.push("Word document".into());
                if self.has_macros {
                    parts.push("macro-enabled".into());
                }
            }
            MagicType::DocExcel | MagicType::DocExcelMacro => {
                parts.push("Excel spreadsheet".into());
                if self.has_macros {
                    parts.push("macro-enabled".into());
                }
            }
            MagicType::DocPowerPoint | MagicType::DocPowerPointMacro => {
                parts.push("PowerPoint presentation".into());
                if self.has_macros {
                    parts.push("macro-enabled".into());
                }
            }

            // Archives
            MagicType::Zip | MagicType::ZipEncrypted => {
                parts.push("ZIP archive".into());
                if self.is_encrypted {
                    parts.push("encrypted".into());
                }
                if let Some(n) = self.file_count {
                    parts.push(format!("{n} files"));
                }
                if let Some(root) = self.zip_root.as_deref() {
                    parts.push(format!("root: {root}"));
                }
            }
            MagicType::Rar => parts.push("RAR archive".into()),
            MagicType::SevenZip => parts.push("7-Zip archive".into()),
            MagicType::Tar => parts.push("TAR archive".into()),
            MagicType::Gzip => parts.push("GZIP archive".into()),
            MagicType::Xz => parts.push("XZ archive".into()),
            MagicType::Bzip2 => parts.push("Bzip2 archive".into()),
            MagicType::Zstd => parts.push("Zstandard archive".into()),

            // App packages
            MagicType::AppJar => {
                parts.push("Java JAR".into());
                if let Some(n) = self.file_count {
                    parts.push(format!("{n} files"));
                }
            }
            MagicType::AppApk => {
                parts.push("Android APK".into());
                if let Some(n) = self.file_count {
                    parts.push(format!("{n} files"));
                }
            }

            // Images
            MagicType::Jpeg
            | MagicType::Png
            | MagicType::Gif
            | MagicType::Bmp
            | MagicType::Webp
            | MagicType::Ico
            | MagicType::Tiff
            | MagicType::Heic => {
                let kind = match self.magic_type {
                    MagicType::Jpeg => "JPEG image",
                    MagicType::Png => "PNG image",
                    MagicType::Gif => "GIF image",
                    MagicType::Bmp => "BMP image",
                    MagicType::Webp => "WebP image",
                    MagicType::Ico => "Icon file",
                    MagicType::Tiff => "TIFF image",
                    MagicType::Heic => "HEIC image",
                    _ => "image",
                };
                parts.push(kind.into());
                if let (Some(w), Some(h)) = (self.width, self.height) {
                    parts.push(format!("{w}\u{00d7}{h}"));
                }
                if self.has_alpha {
                    parts.push("alpha".into());
                }
            }

            // Video
            MagicType::Mp4 | MagicType::Mov | MagicType::Avi | MagicType::Mkv | MagicType::Webm => {
                let kind = match self.magic_type {
                    MagicType::Mp4 => "MP4",
                    MagicType::Mov => "QuickTime",
                    MagicType::Avi => "AVI",
                    MagicType::Mkv => "Matroska",
                    MagicType::Webm => "WebM",
                    _ => "video",
                };
                parts.push(kind.into());
                let track = match (self.has_video, self.has_audio) {
                    (true, true) => Some("video + audio"),
                    (true, false) => Some("video only"),
                    (false, true) => Some("audio only"),
                    (false, false) => None,
                };
                if let Some(t) = track {
                    parts.push(t.into());
                }
            }

            // Windows Media (ASF): WMA when the header carries an audio
            // stream, WMV when it carries a video stream. lofty can't parse
            // ASF, so there are no channel/rate facts to add here — the label
            // alone is enough to keep the file out of the "Binary" bucket (and
            // its false disguise alert).
            MagicType::Asf => {
                let label = match (self.has_video, self.has_audio) {
                    (true, _) => "Windows Media Video",
                    (false, true) => "Windows Media Audio",
                    (false, false) => "Windows Media",
                };
                parts.push(label.into());
            }

            // Audio
            MagicType::Mp3
            | MagicType::Wav
            | MagicType::Flac
            | MagicType::Ogg
            | MagicType::Aiff
            | MagicType::M4a => {
                let kind = match self.magic_type {
                    MagicType::Mp3 => "MP3",
                    MagicType::Wav => "WAV",
                    MagicType::Flac => "FLAC",
                    MagicType::Ogg => "Ogg Vorbis",
                    MagicType::Aiff => "AIFF",
                    MagicType::M4a => "M4A",
                    _ => "audio",
                };
                parts.push(kind.into());
                if let Some(ch) = self.channels {
                    parts.push(if ch == 1 { "mono".into() } else { "stereo".into() });
                }
                if let Some(sr) = self.sample_rate {
                    let khz = sr as f32 / 1000.0;
                    if (khz - khz.floor()).abs() < f32::EPSILON {
                        parts.push(format!("{} kHz", khz as u32));
                    } else {
                        parts.push(format!("{khz:.1} kHz"));
                    }
                }
                if let Some(kbps) = self.bitrate_kbps {
                    parts.push(format!("{kbps} kbps"));
                }
                if let Some(secs) = self.duration_secs {
                    let mins = secs / 60;
                    let rem = secs % 60;
                    parts.push(format!("{mins:02}:{rem:02}"));
                }
            }

            // Data formats with no extra facts
            MagicType::Pdf => parts.push("PDF document".into()),
            MagicType::Sqlite => parts.push("SQLite database".into()),
            MagicType::Json => parts.push("JSON data".into()),
            MagicType::Xml => parts.push("XML document".into()),
            MagicType::Html => parts.push("HTML document".into()),
            MagicType::Svg => parts.push("XML / SVG".into()),
            MagicType::TextIni => parts.push("INI configuration".into()),
            MagicType::TextReg => parts.push("Windows Registry export".into()),
            MagicType::TextPlain => parts.push("Plain text".into()),
            MagicType::Utf8Bom | MagicType::Utf16Le | MagicType::Utf16Be => {
                parts.push("Text".into())
            }

            // Scripts
            MagicType::ScriptBash => {
                parts.push("Shell script".into());
                if let Some(i) = self.interpreter {
                    parts.push(i.into());
                }
            }
            MagicType::ScriptPython => parts.push("Python script".into()),
            MagicType::ScriptPerl => parts.push("Perl script".into()),
            MagicType::ScriptRuby => parts.push("Ruby script".into()),
            MagicType::ScriptNode => parts.push("Node.js script".into()),
            MagicType::ScriptOther => parts.push("Script".into()),

            // Windows shortcuts
            MagicType::Lnk => parts.push("Windows shortcut".into()),
            MagicType::Url => parts.push("Internet shortcut".into()),

            // Fonts
            MagicType::OpenType => parts.push("OpenType font".into()),
            MagicType::TrueType => parts.push("TrueType font".into()),

            // Misc
            MagicType::MachOFat => parts.push("Mach-O fat binary".into()),
            MagicType::Binary => parts.push("Binary data".into()),
            MagicType::Folder | MagicType::Unknown => {}
        }

        parts.join(" \u{00b7} ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_for_unknown_and_folder() {
        assert!(MagicInfo::new(MagicType::Unknown).description().is_empty());
        assert!(MagicInfo::new(MagicType::Folder).description().is_empty());
    }

    #[test]
    fn pe_description_combines_facts() {
        let mut info = MagicInfo::new(MagicType::ExeWindowsNet);
        info.is_64bit = Some(true);
        info.arch = CpuArch::X64;
        info.subsystem = PeSubsystem::Gui;
        info.is_dotnet = true;
        let desc = info.description();
        assert!(desc.contains("Windows PE"));
        assert!(desc.contains("64-bit"));
        assert!(desc.contains("x86-64"));
        assert!(desc.contains("GUI"));
        assert!(desc.contains(".NET"));
    }

    #[test]
    fn aros_elf_description_names_os_and_relocatable() {
        let mut info = MagicInfo::new(MagicType::ExeLinux);
        info.is_64bit = Some(true);
        info.arch = CpuArch::Arm64;
        info.os = ElfOs::Aros;
        info.is_relocatable = true;
        assert_eq!(
            info.description(),
            "ELF \u{b7} 64-bit \u{b7} relocatable \u{b7} ARM64 \u{b7} AROS"
        );
    }

    #[test]
    fn plain_linux_elf_has_no_os_suffix() {
        // System V / "none" OSABI (the GNU/Linux default) → no OS tag,
        // preserving the pre-AROS description shape.
        let mut info = MagicInfo::new(MagicType::ExeLinux);
        info.is_64bit = Some(true);
        info.arch = CpuArch::X64;
        assert_eq!(info.description(), "ELF \u{b7} 64-bit \u{b7} executable \u{b7} x86-64");
    }

    #[test]
    fn image_description_has_dimensions() {
        let mut info = MagicInfo::new(MagicType::Png);
        info.width = Some(1920);
        info.height = Some(1080);
        info.has_alpha = true;
        assert_eq!(info.description(), "PNG image \u{00b7} 1920\u{00d7}1080 \u{00b7} alpha");
    }

    #[test]
    fn mp3_description_has_audio_facts() {
        let mut info = MagicInfo::new(MagicType::Mp3);
        info.channels = Some(2);
        info.sample_rate = Some(44100);
        info.bitrate_kbps = Some(192);
        info.duration_secs = Some(204);
        let desc = info.description();
        assert!(desc.contains("MP3"));
        assert!(desc.contains("stereo"));
        // 44 100 Hz is not a whole kHz, so the formatter picks the
        // one-decimal-place branch: "44.1 kHz", not "44 kHz".
        assert!(desc.contains("44.1 kHz"));
        assert!(desc.contains("192 kbps"));
        assert!(desc.contains("03:24"));
    }

    #[test]
    fn display_name_preserves_legacy_labels() {
        assert_eq!(MagicType::Png.display_name(), "PNG image");
        assert_eq!(MagicType::Zip.display_name(), "ZIP archive");
        assert_eq!(MagicType::ExeWindows.display_name(), "PE / DOS executable");
        assert_eq!(MagicType::ExeLinux.display_name(), "ELF executable");
    }
}
