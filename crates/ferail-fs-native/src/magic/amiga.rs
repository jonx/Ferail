//! AmigaOS-family formats: hunk binaries, Workbench icons, IFF containers,
//! tracker modules and disk images.
//!
//! These are cross-platform sniffers, not an AROS feature. An Aminet download
//! sitting on a Mac is still an `.lha` full of `.info` icons, ILBM images and
//! ProTracker modules, and until now every one of them landed in the
//! Description column as nothing at all. They are pure header reads, so the
//! same code answers on macOS, Windows, Linux and AROS.
//!
//! Everything here is big-endian: the formats were designed on a 68000.
//!
//! # Ordering
//!
//! [`sniff`] runs after the ELF/Mach-O/PE parser and before the generic
//! signature table. It deliberately does **not** claim `FORM….AIFF`/`AIFC` —
//! those are IFF too, but [`super::audio`] already parses them properly and
//! reports channels and duration.

use super::types::{CpuArch, MagicInfo, MagicType};

/// Big-endian u32 at `off`, or `None` when the buffer is too short.
fn be_u32(buf: &[u8], off: usize) -> Option<u32> {
    let b = buf.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Big-endian u16 at `off`.
fn be_u16(buf: &[u8], off: usize) -> Option<u16> {
    let b = buf.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if let Some(info) = sniff_hunk(buf) {
        return Some(info);
    }
    if let Some(info) = sniff_icon(buf) {
        return Some(info);
    }
    if let Some(info) = sniff_iff(buf) {
        return Some(info);
    }
    if let Some(info) = sniff_tracker(buf) {
        return Some(info);
    }
    sniff_misc(buf)
}

/// AmigaOS hunk binaries — the native 68k executable format, which is not ELF
/// and so was previously invisible to the detector.
///
/// | Magic | Meaning |
/// |---|---|
/// | `0x000003F3` | `HUNK_HEADER` — a loadable program |
/// | `0x000003E7` | `HUNK_UNIT` — a linker object |
/// | `0x000003FA` | `HUNK_LIB` — a link library |
///
/// After `HUNK_HEADER` comes a (usually empty) resident-library name list
/// terminated by a zero longword, then the hunk table size, and the first and
/// last hunk indices. The table size doubles as the hunk count, which is worth
/// reporting: it is the one cheap structural fact a hunk file offers.
fn sniff_hunk(buf: &[u8]) -> Option<MagicInfo> {
    let magic = be_u32(buf, 0)?;
    let magic_type = match magic {
        0x0000_03F3 => MagicType::ExeAmiga,
        0x0000_03E7 => MagicType::ObjAmiga,
        0x0000_03FA => MagicType::LibAmiga,
        _ => return None,
    };
    let mut info = MagicInfo::new(magic_type);
    // Hunk binaries are 68k by construction; PowerUP/WarpOS extensions ride
    // inside the same container but are not distinguishable from the header.
    info.arch = CpuArch::M68k;

    if magic_type == MagicType::ExeAmiga {
        // Skip the resident-library name list: a sequence of length-prefixed
        // names (length in longwords) ending with a zero longword. Almost
        // always immediately zero.
        let mut off = 4;
        loop {
            let len = be_u32(buf, off)?;
            off += 4;
            if len == 0 {
                break;
            }
            // Guard against a corrupt/hostile length walking us off the end.
            off = off.checked_add((len as usize).checked_mul(4)?)?;
            if off >= buf.len() {
                return Some(info);
            }
        }
        if let Some(table_size) = be_u32(buf, off) {
            if table_size > 0 && table_size < 10_000 {
                info.hunk_count = Some(table_size);
            }
        }
    }
    Some(info)
}

/// Workbench `.info` icons — `DiskObject`, magic `0xE310`.
///
/// Layout (all big-endian):
///
/// ```text
///  0  UWORD  do_Magic = 0xE310
///  2  UWORD  do_Version
///  4  struct Gadget do_Gadget  (44 bytes)
///       +8   WORD  LeftEdge     -> absolute 12
///       +10  WORD  TopEdge      -> absolute 14  (Width/Height follow)
/// 48  UBYTE  do_Type
/// ```
///
/// `Gadget` is `NextGadget` (4) then LeftEdge/TopEdge/Width/Height, so the
/// icon's pixel size sits at absolute offsets 12 and 14.
fn sniff_icon(buf: &[u8]) -> Option<MagicInfo> {
    if be_u16(buf, 0)? != 0xE310 {
        return None;
    }
    let mut info = MagicInfo::new(MagicType::AmigaIcon);
    // Width/Height of the gadget's hit box == the icon image size.
    let w = be_u16(buf, 12).unwrap_or(0);
    let h = be_u16(buf, 14).unwrap_or(0);
    // Sanity-bound them: a corrupt icon should not print an absurd size.
    if w > 0 && h > 0 && w <= 4096 && h <= 4096 {
        info.width = Some(w as u32);
        info.height = Some(h as u32);
    }
    info.icon_kind = buf.get(48).copied().filter(|t| (1..=8).contains(t));
    Some(info)
}

/// IFF containers: `FORM` + length + form type.
///
/// Handles the Amiga-native form types. `AIFF`/`AIFC` are deliberately left to
/// [`super::audio`], which reports channels and duration for them.
fn sniff_iff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() < 12 || &buf[0..4] != b"FORM" {
        return None;
    }
    let form = &buf[8..12];
    match form {
        b"ILBM" | b"PBM " | b"ACBM" => {
            let mut info = MagicInfo::new(MagicType::Ilbm);
            // BMHD is required to be the first chunk: `BMHD`, u32 size, then
            // w/h/x/y as u16 and nPlanes as u8.
            if buf.get(12..16) == Some(b"BMHD") {
                info.width = be_u16(buf, 20).map(u32::from);
                info.height = be_u16(buf, 22).map(u32::from);
                // Bit depth: 1..8 planes normally, 24 for deep ILBM.
                info.bit_depth = buf.get(28).copied().filter(|d| *d > 0 && *d <= 32);
            }
            Some(info)
        }
        b"8SVX" | b"16SV" => {
            let mut info = MagicInfo::new(MagicType::Svx8);
            // VHDR: 3 u32 sample counts, then samplesPerSec as u16.
            if buf.get(12..16) == Some(b"VHDR") {
                info.sample_rate = be_u16(buf, 32).map(u32::from).filter(|r| *r > 0);
            }
            // 8SVX is mono by definition (stereo uses two FORMs in a LIST).
            info.channels = Some(1);
            Some(info)
        }
        b"ANIM" => {
            let mut info = MagicInfo::new(MagicType::IffAnim);
            info.has_video = true;
            Some(info)
        }
        b"SMUS" => Some(MagicInfo::new(MagicType::IffSmus)),
        _ => None,
    }
}

/// Tracker modules — the Amiga's other ubiquitous format.
///
/// ProTracker-family modules carry a 4-byte tag at offset **1080**, past the
/// 20-byte title and 31 sample records; the tag encodes the channel count.
/// The later PC trackers (XM/S3M/IT) and Amiga's own OctaMED are identified
/// from offset 0 (except S3M, whose `SCRM` tag sits at 44).
fn sniff_tracker(buf: &[u8]) -> Option<MagicInfo> {
    let mut info = MagicInfo::new(MagicType::TrackerModule);

    if let Some(tag) = buf.get(1080..1084) {
        let channels: Option<u8> = match tag {
            b"M.K." | b"M!K!" | b"FLT4" | b"4CHN" | b"M&K!" | b"N.T." => Some(4),
            b"6CHN" => Some(6),
            b"8CHN" | b"FLT8" | b"OCTA" | b"CD81" => Some(8),
            b"16CN" => Some(16),
            b"32CN" => Some(32),
            _ => None,
        };
        if let Some(ch) = channels {
            info.channels = Some(ch);
            info.tracker_kind = Some("ProTracker");
            return Some(info);
        }
    }

    if buf.starts_with(b"Extended Module: ") {
        info.tracker_kind = Some("FastTracker XM");
        return Some(info);
    }
    if buf.starts_with(b"IMPM") {
        info.tracker_kind = Some("Impulse Tracker");
        return Some(info);
    }
    if buf.get(44..48) == Some(b"SCRM") {
        info.tracker_kind = Some("ScreamTracker 3");
        return Some(info);
    }
    // OctaMED / MED — Amiga native, `MMD0`..`MMD3`.
    if let Some(tag) = buf.get(0..4) {
        if tag.starts_with(b"MMD") && matches!(tag[3], b'0'..=b'3') {
            info.tracker_kind = Some("OctaMED");
            return Some(info);
        }
    }
    None
}

/// Disk images, the LZX archiver, and AmigaGuide hypertext.
fn sniff_misc(buf: &[u8]) -> Option<MagicInfo> {
    // ADF floppy image: the bootblock starts `DOS` plus a filesystem flag —
    // 0 = OFS, 1 = FFS, and 2..7 the international/dir-cache variants.
    if buf.starts_with(b"DOS") {
        if let Some(flag) = buf.get(3) {
            if *flag <= 7 {
                return Some(MagicInfo::new(MagicType::AdfDisk));
            }
        }
    }
    if buf.starts_with(b"DMS!") {
        return Some(MagicInfo::new(MagicType::DmsDisk));
    }
    if buf.starts_with(b"LZX") {
        return Some(MagicInfo::new(MagicType::Lzx));
    }
    if buf.starts_with(b"@database") {
        return Some(MagicInfo::new(MagicType::AmigaGuide));
    }
    None
}
