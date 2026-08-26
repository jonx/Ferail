//! Content-first recognition for NFO/DIZ and checksum sidecars.

use ferail_core::text_encoding::{
    decode_text, looks_like_cp437_art, looks_like_text_art, TextEncoding,
};

use super::types::{MagicInfo, MagicType};

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    let decoded = decode_text(buf)?;
    let text = decoded.text.trim_start_matches('\u{feff}');

    if looks_like_msinfo(text) {
        let mut info = MagicInfo::new(MagicType::NfoMsInfo);
        info.text_encoding = Some(encoding_name(decoded.encoding));
        return Some(info);
    }
    if looks_like_kodi(text) {
        let mut info = MagicInfo::new(MagicType::NfoKodi);
        info.text_encoding = Some(encoding_name(decoded.encoding));
        return Some(info);
    }
    if let Some(algorithm) = checksum_list_algorithm(text) {
        let mut info = MagicInfo::new(MagicType::ChecksumList);
        info.checksum_algorithm = Some(algorithm);
        info.text_encoding = Some(encoding_name(decoded.encoding));
        return Some(info);
    }
    if looks_like_sfv(text) {
        let mut info = MagicInfo::new(MagicType::ChecksumSfv);
        info.checksum_algorithm = Some("CRC32");
        info.text_encoding = Some(encoding_name(decoded.encoding));
        return Some(info);
    }
    if looks_like_cp437_art(buf) || looks_like_text_art(text) {
        let mut info = MagicInfo::new(MagicType::NfoScene);
        info.text_encoding = Some(encoding_name(decoded.encoding));
        return Some(info);
    }
    None
}

fn encoding_name(encoding: TextEncoding) -> &'static str {
    match encoding {
        TextEncoding::Utf8 => "UTF-8",
        TextEncoding::Utf16Le => "UTF-16 LE",
        TextEncoding::Utf16Be => "UTF-16 BE",
        TextEncoding::Cp437 => "CP437",
        TextEncoding::Latin1 => "Latin-1",
    }
}

fn first_xml_root(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    loop {
        let open = rest.find('<')?;
        rest = &rest[open + 1..];
        if rest.starts_with('?') {
            rest = rest.get(rest.find("?>")? + 2..)?;
            continue;
        }
        if rest.starts_with("!--") {
            rest = rest.get(rest.find("-->")? + 3..)?;
            continue;
        }
        if rest.starts_with('!') {
            rest = rest.get(rest.find('>')? + 1..)?;
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
            .collect();
        return (!name.is_empty()).then(|| {
            name.rsplit(':')
                .next()
                .unwrap_or(&name)
                .to_ascii_lowercase()
        });
    }
}

fn looks_like_msinfo(text: &str) -> bool {
    first_xml_root(text).as_deref() == Some("msinfo")
}

fn looks_like_kodi(text: &str) -> bool {
    const ROOTS: &[&str] = &[
        "movie",
        "movieset",
        "set",
        "tvshow",
        "episodedetails",
        "musicvideo",
        "artist",
        "album",
    ];
    if first_xml_root(text).is_some_and(|root| ROOTS.contains(&root.as_str())) {
        return true;
    }

    let mut meaningful = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = meaningful.next() else {
        return false;
    };
    meaningful.next().is_none() && is_kodi_scraper_url(first)
}

fn is_kodi_scraper_url(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && [
            "themoviedb.org/",
            "thetvdb.com/",
            "tvdb.com/",
            "imdb.com/title/",
            "musicbrainz.org/",
        ]
        .iter()
        .any(|host| lower.contains(host))
}

fn looks_like_sfv(text: &str) -> bool {
    let mut valid = 0usize;
    let mut invalid = 0usize;
    for line in text.lines().take(200) {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if parse_sfv_line(line) {
            valid += 1;
        } else {
            invalid += 1;
        }
    }
    valid >= 1 && invalid == 0
}

fn parse_sfv_line(line: &str) -> bool {
    let Some(split) = line.rfind(char::is_whitespace) else {
        return false;
    };
    let name = line[..split].trim_end();
    let crc = line[split..].trim();
    !name.is_empty() && crc.len() == 8 && crc.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn checksum_list_algorithm(text: &str) -> Option<&'static str> {
    let mut algorithm = None;
    let mut valid = 0usize;
    let mut invalid = 0usize;
    for line in text.lines().take(200) {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        match parse_checksum_algorithm(line) {
            Some(found) if algorithm.is_none() || algorithm == Some(found) => {
                algorithm = Some(found);
                valid += 1;
            }
            Some(_) | None => invalid += 1,
        }
    }
    (valid >= 1 && invalid == 0).then_some(algorithm).flatten()
}

fn parse_checksum_algorithm(line: &str) -> Option<&'static str> {
    let unescaped = line.strip_prefix('\\').unwrap_or(line);
    if let Some((digest, rest)) = unescaped.split_once(' ') {
        if !rest.is_empty()
            && matches!(rest.as_bytes().first(), Some(b' ' | b'*'))
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return algorithm_for_hex_len(digest.len());
        }
    }

    let open = line.find(" (")?;
    let close = line.rfind(") = ")?;
    if close <= open + 2 {
        return None;
    }
    let named = line[..open].trim().to_ascii_uppercase();
    let digest = line[close + 4..].trim();
    if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    match (named.as_str(), digest.len()) {
        ("MD5", 32) => Some("MD5"),
        ("SHA1" | "SHA-1", 40) => Some("SHA-1"),
        ("SHA224" | "SHA-224", 56) => Some("SHA-224"),
        ("SHA256" | "SHA-256", 64) => Some("SHA-256"),
        ("SHA384" | "SHA-384", 96) => Some("SHA-384"),
        ("SHA512" | "SHA-512", 128) => Some("SHA-512"),
        _ => None,
    }
}

fn algorithm_for_hex_len(len: usize) -> Option<&'static str> {
    match len {
        32 => Some("MD5"),
        40 => Some("SHA-1"),
        56 => Some("SHA-224"),
        64 => Some("SHA-256"),
        96 => Some("SHA-384"),
        128 => Some("SHA-512"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../test-data/sidecars/generated")
                .join(path),
        )
        .unwrap()
    }

    #[test]
    fn classifies_fixture_corpus() {
        let cases = [
            ("nfo/scene-cp437.nfo", MagicType::NfoScene),
            ("nfo/scene-ansi.nfo", MagicType::NfoScene),
            ("nfo/ferail-release-color.nfo", MagicType::NfoScene),
            ("nfo/scene-utf8.nfo", MagicType::NfoScene),
            ("nfo/kodi-metadata.nfo", MagicType::NfoKodi),
            ("nfo/kodi-url.nfo", MagicType::NfoKodi),
            ("nfo/kodi-combined.nfo", MagicType::NfoKodi),
            ("nfo/kodi-artist.nfo", MagicType::NfoKodi),
            ("nfo/msinfo.nfo", MagicType::NfoMsInfo),
            ("manifests/release.sfv", MagicType::ChecksumSfv),
            ("manifests/SHA256SUMS", MagicType::ChecksumList),
            ("manifests/BSD-SHA256", MagicType::ChecksumList),
        ];
        for (path, expected) in cases {
            assert_eq!(
                sniff(&fixture(path)).unwrap().magic_type,
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn ordinary_text_and_malformed_manifests_decline() {
        for path in [
            "negative/french-latin1.txt",
            "negative/generic.xml",
            "negative/plain.nfo",
            "manifests/malformed.sfv",
            "manifests/MIXEDSUMS",
        ] {
            assert!(sniff(&fixture(path)).is_none(), "{path}");
        }
    }
}
