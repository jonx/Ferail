//! Executable parsers: PE (Windows), ELF (Linux), Mach-O (macOS).
//!
//! All three formats keep their critical metadata in fixed-offset
//! header structures that fit inside the 4 KB read budget:
//!
//! - ELF needs the first 20 bytes (e_ident + e_type + e_machine).
//! - Mach-O needs the first 16 bytes (magic + cputype + filetype).
//! - PE needs `pe_offset` from byte 0x3C, then optional-header /
//!   subsystem / data-directory fields at relative offsets up to
//!   ~0x100 from `pe_offset`. Typical `pe_offset` is < 0x100, so
//!   the deepest read lands around 0x200 — well within 4 KB.
//!
//! Ported from bfe-explorer's `sniff_executable_info`,
//! `sniff_elf_info`, `sniff_macho_info`, `sniff_pe_info`.

use super::types::{CpuArch, ElfOs, MagicInfo, MagicType, PeSubsystem};

/// Dispatch into PE / ELF / Mach-O if the buffer's prefix matches.
pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() >= 20 && buf.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return Some(sniff_elf(buf));
    }

    if buf.len() >= 8 {
        let magic = &buf[0..4];
        if matches!(
            magic,
            &[0xfe, 0xed, 0xfa, 0xce]
                | &[0xfe, 0xed, 0xfa, 0xcf]
                | &[0xce, 0xfa, 0xed, 0xfe]
                | &[0xcf, 0xfa, 0xed, 0xfe]
                | &[0xca, 0xfe, 0xba, 0xbe]
        ) {
            return Some(sniff_macho(buf));
        }
    }

    if buf.len() >= 64 && buf.starts_with(b"MZ") {
        return Some(sniff_pe(buf));
    }

    None
}

/// ELF header layout (offsets):
///
/// - 4: e_ident[EI_CLASS] (1 = 32-bit, 2 = 64-bit)
/// - 5: e_ident[EI_DATA]  (1 = little-endian, 2 = big-endian)
/// - 7: e_ident[EI_OSABI] (0 = System V, 15 = AROS, …)
/// - 16-17: e_type (relocatable / executable / shared object)
/// - 18-19: e_machine (architecture)
fn sniff_elf(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::ExeLinux);
    if buf.len() < 20 {
        return info;
    }

    info.is_64bit = Some(buf[4] == 2);
    let is_le = buf[5] == 1;

    // EI_OSABI — most GNU/Linux binaries leave this 0 (System V); a named
    // OS here is a deliberate marker. AROS stamps ELFOSABI_AROS (15).
    info.os = match buf[7] {
        2 => ElfOs::NetBsd,
        3 => ElfOs::Linux,
        6 => ElfOs::Solaris,
        9 => ElfOs::FreeBsd,
        12 => ElfOs::OpenBsd,
        15 => ElfOs::Aros,
        _ => ElfOs::Unknown,
    };

    let read_u16 = |offset: usize| -> u16 {
        if is_le {
            u16::from_le_bytes([buf[offset], buf[offset + 1]])
        } else {
            u16::from_be_bytes([buf[offset], buf[offset + 1]])
        }
    };

    // e_type: ET_REL (1) is a relocatable object — not a runnable image,
    // but the form AROS uses to ship its libraries. Keep the ExeLinux
    // family label (so the icon classifier still tints it as an
    // executable) and flag the kind for the description.
    let e_type = read_u16(16);
    info.is_relocatable = e_type == 1;
    info.magic_type = match e_type {
        3 => MagicType::SoLinux, // ET_DYN
        _ => MagicType::ExeLinux,
    };

    let e_machine = read_u16(18);
    info.arch = match e_machine {
        0x03 => CpuArch::X86,
        0x3e => CpuArch::X64,
        0x28 => CpuArch::Arm,
        0xb7 => CpuArch::Arm64,
        0xf3 => CpuArch::Riscv,
        0x14 | 0x15 => CpuArch::PowerPc,
        0x04 => CpuArch::M68k,
        0x08 => CpuArch::Mips,
        _ => CpuArch::Unknown,
    };

    info
}

/// Mach-O header:
///
/// - 0-3: magic (determines 32/64-bit + endianness)
/// - 4-7: cputype
/// - 12-15: filetype
fn sniff_macho(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::ExeMac);
    if buf.len() < 8 {
        return info;
    }

    let magic = &buf[0..4];
    let is_64bit = matches!(magic, &[0xfe, 0xed, 0xfa, 0xcf] | &[0xcf, 0xfa, 0xed, 0xfe]);
    // `ce fa ed fe` / `cf fa ed fe` are the byte sequences produced by a
    // little-endian Mach-O header. The old `is_swapped` naming had this
    // backwards and happened to classify ordinary files from palindromic-ish
    // low file-type values as the default executable.
    let is_little_endian = matches!(magic, &[0xce, 0xfa, 0xed, 0xfe] | &[0xcf, 0xfa, 0xed, 0xfe]);
    let is_fat = magic == [0xca, 0xfe, 0xba, 0xbe];

    if is_fat {
        info.magic_type = MagicType::MachOFat;
        return info;
    }

    info.is_64bit = Some(is_64bit);

    if buf.len() >= 8 {
        let cputype = if is_little_endian {
            u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]])
        } else {
            u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]])
        };
        info.arch = match cputype & 0xff {
            7 => {
                if is_64bit {
                    CpuArch::X64
                } else {
                    CpuArch::X86
                }
            }
            12 => {
                if is_64bit {
                    CpuArch::Arm64
                } else {
                    CpuArch::Arm
                }
            }
            18 => CpuArch::PowerPc,
            _ => CpuArch::Unknown,
        };
    }

    if buf.len() >= 16 {
        let filetype = if is_little_endian {
            u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]])
        } else {
            u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]])
        };
        info.magic_type = match filetype {
            0x1 => MagicType::ObjMac,   // MH_OBJECT
            0x2 => MagicType::ExeMac,   // MH_EXECUTE
            0x6 => MagicType::DylibMac, // MH_DYLIB
            0x8 => MagicType::DylibMac, // MH_BUNDLE — treat as dylib
            _ => MagicType::ExeMac,
        };
    }

    info
}

/// PE header layout (all multi-byte values are little-endian):
///
/// - 0x3C: u32 offset to "PE\0\0" signature (`pe_offset`)
/// - `pe_offset + 0`: "PE\0\0"
/// - `pe_offset + 4`: COFF header
///     - +0  Machine (u16)
///     - +18 Characteristics (u16, bit 0x2000 = IMAGE_FILE_DLL)
/// - `pe_offset + 24`: Optional header
///     - +0  Magic (0x10b = PE32, 0x20b = PE32+ / 64-bit)
///     - +68 (PE32) / +88 (PE32+): Subsystem (u16)
///     - +96 (PE32) / +112 (PE32+): start of DataDirectory[16]
///       - +14*8 = CLR runtime header data dir (RVA + Size, 8 bytes)
fn sniff_pe(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::ExeWindows);
    if buf.len() < 64 {
        return info;
    }

    let pe_offset = u32::from_le_bytes([buf[0x3c], buf[0x3d], buf[0x3e], buf[0x3f]]) as usize;

    if buf.len() < pe_offset + 4 || &buf[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return info;
    }

    // Machine
    if buf.len() >= pe_offset + 6 {
        let machine = u16::from_le_bytes([buf[pe_offset + 4], buf[pe_offset + 5]]);
        info.arch = match machine {
            0x014c => CpuArch::X86,
            0x8664 => CpuArch::X64,
            0x01c0 | 0x01c4 => CpuArch::Arm,
            0xaa64 => CpuArch::Arm64,
            _ => CpuArch::Unknown,
        };
    }

    // Characteristics
    if buf.len() < pe_offset + 24 {
        return info;
    }
    let characteristics = u16::from_le_bytes([buf[pe_offset + 22], buf[pe_offset + 23]]);
    let is_dll = (characteristics & 0x2000) != 0;

    // Optional header magic
    if buf.len() < pe_offset + 26 {
        info.magic_type = if is_dll {
            MagicType::DllWindows
        } else {
            MagicType::ExeWindows
        };
        return info;
    }
    let optional_magic = u16::from_le_bytes([buf[pe_offset + 24], buf[pe_offset + 25]]);
    let is_pe32plus = optional_magic == 0x20b;
    info.is_64bit = Some(is_pe32plus);

    // Subsystem
    let subsystem_offset = pe_offset + 24 + if is_pe32plus { 88 } else { 68 };
    if buf.len() >= subsystem_offset + 2 {
        let subsystem = u16::from_le_bytes([buf[subsystem_offset], buf[subsystem_offset + 1]]);
        info.subsystem = match subsystem {
            1 => PeSubsystem::Native,
            2 => PeSubsystem::Gui,
            3 => PeSubsystem::Console,
            _ => PeSubsystem::Unknown,
        };
    }

    // CLR (.NET) data directory entry
    let data_dir_offset = pe_offset + 24 + if is_pe32plus { 112 } else { 96 };
    let clr_dir_offset = data_dir_offset + (14 * 8);
    if buf.len() >= clr_dir_offset + 8 {
        let clr_rva = u32::from_le_bytes([
            buf[clr_dir_offset],
            buf[clr_dir_offset + 1],
            buf[clr_dir_offset + 2],
            buf[clr_dir_offset + 3],
        ]);
        let clr_size = u32::from_le_bytes([
            buf[clr_dir_offset + 4],
            buf[clr_dir_offset + 5],
            buf[clr_dir_offset + 6],
            buf[clr_dir_offset + 7],
        ]);
        if clr_rva != 0 && clr_size != 0 {
            info.is_dotnet = true;
            info.magic_type = MagicType::ExeWindowsNet;
            return info;
        }
    }

    info.magic_type = if is_dll {
        MagicType::DllWindows
    } else {
        MagicType::ExeWindows
    };
    info
}
