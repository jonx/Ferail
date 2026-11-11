//! Cover images for e-books and comic archives.
//!
//! An EPUB is a zip whose package document names its cover; a comic archive
//! (`.cbz` zip, `.cbr` RAR, `.cbt` tar) is a bag of page images whose first
//! page, in natural name order, is the cover. Both come back as the raw
//! **encoded** image bytes for the host to decode, like
//! [`crate::media::read_cover_art`].
//!
//! The container is opened by content (`archive::probe_format`), so a
//! `.cbr` that is really a zip, a common mislabel, still reads.

use std::path::Path;

use crate::archive;

/// Extensions [`read_cover`] answers for. Lowercase.
const EBOOK_EXTS: &[&str] = &["epub", "cbz", "cbr", "cbt"];

/// Page/cover encodings the host decoders handle.
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp"];

/// Largest cover image read into memory.
const COVER_CAP: u64 = 64 * 1024 * 1024;
/// Largest EPUB container/package document read into memory.
const XML_CAP: u64 = 4 * 1024 * 1024;

/// Whether `ext` (no dot, any case) is an e-book or comic-archive extension.
/// Lexical only.
pub fn is_ebook_extension(ext: &str) -> bool {
    EBOOK_EXTS.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

/// Whether `path` names an e-book or comic archive. Lexical only.
pub fn is_ebook_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(is_ebook_extension)
}

/// Read the cover of the e-book or comic archive at `path`: the encoded
/// image bytes, or `None` when the file is not one, is encrypted, or holds
/// no usable image. Blocking (opens and decompresses from the archive):
/// off the UI thread only.
pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    ferail_core::path_guard::assert_off_ui_thread("ebook::read_cover");
    if !is_ebook_path(path) {
        return None;
    }
    let toc = archive::read_toc(path, None).ok()?;
    let files: Vec<&str> = toc
        .entries
        .iter()
        .filter(|entry| !entry.is_dir && !entry.encrypted)
        .map(|entry| entry.path.as_str())
        .collect();
    let member = epub_cover(path, &files).or_else(|| first_page(&files))?;
    archive::read_entry_bytes(path, &member, None, COVER_CAP)
        .ok()
        .flatten()
        .filter(|bytes| !bytes.is_empty())
}

fn extension_of(name: &str) -> Option<&str> {
    let leaf = name.rsplit('/').next().unwrap_or(name);
    leaf.rsplit_once('.').map(|(_, ext)| ext)
}

fn is_image_name(name: &str) -> bool {
    extension_of(name).is_some_and(|ext| IMAGE_EXTS.iter().any(|known| known.eq_ignore_ascii_case(ext)))
}

/// The first page of a comic archive: the first image in natural path
/// order, ignoring the resource-fork litter macOS zips carry.
fn first_page(files: &[&str]) -> Option<String> {
    files
        .iter()
        .filter(|name| is_image_name(name))
        .filter(|name| {
            let leaf = name.rsplit('/').next().unwrap_or(name);
            !name.starts_with("__MACOSX/") && !leaf.starts_with('.')
        })
        .min_by_key(|name| ferail_core::natural::natural_key(name))
        .map(|name| name.to_string())
}

/// The cover member an EPUB's package document declares, when the archive
/// is an EPUB and the declared file is present.
fn epub_cover(archive_path: &Path, files: &[&str]) -> Option<String> {
    const CONTAINER: &str = "META-INF/container.xml";
    if !files.contains(&CONTAINER) {
        return None;
    }
    let container = read_text(archive_path, CONTAINER)?;
    let opf_path = rootfile_path(&container)?;
    let opf = read_text(archive_path, &opf_path)?;
    let href = opf_cover_href(&opf)?;
    let wanted = resolve_href(&opf_path, &href);
    files
        .iter()
        .find(|name| **name == wanted)
        .or_else(|| files.iter().find(|name| name.eq_ignore_ascii_case(&wanted)))
        .map(|name| name.to_string())
}

fn read_text(archive_path: &Path, member: &str) -> Option<String> {
    let bytes = archive::read_entry_bytes(archive_path, member, None, XML_CAP).ok()??;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.trim_start_matches('\u{feff}').to_string())
}

fn parse_xml(xml: &str) -> Option<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    roxmltree::Document::parse_with_options(xml, options).ok()
}

/// The package document path from `META-INF/container.xml`.
fn rootfile_path(container_xml: &str) -> Option<String> {
    let doc = parse_xml(container_xml)?;
    doc.descendants()
        .filter(|node| node.tag_name().name() == "rootfile")
        .find_map(|node| node.attribute("full-path"))
        .filter(|path| !path.is_empty())
        .map(|path| path.to_string())
}

/// The cover image's `href` from an OPF package document, tried in order:
/// the EPUB 3 `cover-image` manifest property, the EPUB 2
/// `<meta name="cover">` pointer, a manifest image named like a cover, and
/// finally the first manifest image.
fn opf_cover_href(opf_xml: &str) -> Option<String> {
    let doc = parse_xml(opf_xml)?;
    struct Item<'a> {
        id: &'a str,
        href: &'a str,
        properties: &'a str,
    }
    let images: Vec<Item<'_>> = doc
        .descendants()
        .filter(|node| node.tag_name().name() == "item")
        .filter_map(|node| {
            let href = node.attribute("href")?;
            is_image_name(href.split('#').next().unwrap_or(href)).then(|| Item {
                id: node.attribute("id").unwrap_or(""),
                href,
                properties: node.attribute("properties").unwrap_or(""),
            })
        })
        .collect();

    let by_property = || {
        images
            .iter()
            .find(|item| item.properties.split_whitespace().any(|p| p == "cover-image"))
    };
    let by_meta = || {
        let cover_id = doc
            .descendants()
            .filter(|node| node.tag_name().name() == "meta")
            .find(|node| node.attribute("name") == Some("cover"))
            .and_then(|node| node.attribute("content"))?;
        images.iter().find(|item| item.id == cover_id)
    };
    let by_name = || {
        images.iter().find(|item| {
            item.id.to_ascii_lowercase().contains("cover")
                || item.href.to_ascii_lowercase().contains("cover")
        })
    };
    by_property()
        .or_else(by_meta)
        .or_else(by_name)
        .or_else(|| images.first())
        .map(|item| item.href.to_string())
}

/// Resolve a manifest `href` (a relative, percent-encoded URL) against the
/// package document's own path, to an archive member path.
fn resolve_href(opf_path: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let decoded = percent_decode(href);
    let mut parts: Vec<&str> = match opf_path.rsplit_once('/') {
        Some((dir, _)) if !decoded.starts_with('/') => dir.split('/').collect(),
        _ => Vec::new(),
    };
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in members {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ferail-ebook-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extensions_are_case_insensitive() {
        assert!(is_ebook_path(Path::new("a/Book.EPUB")));
        assert!(is_ebook_path(Path::new("issue 01.cbr")));
        assert!(!is_ebook_path(Path::new("archive.zip")));
        assert!(!is_ebook_path(Path::new("epub")));
    }

    #[test]
    fn comic_cover_is_first_page_in_natural_order() {
        let dir = scratch("cbz");
        let path = dir.join("issue.cbz");
        write_zip(
            &path,
            &[
                ("__MACOSX/._page1.jpg", b"fork"),
                ("pages/page10.jpg", b"ten"),
                ("pages/page2.jpg", b"two"),
                ("pages/credits.txt", b"text"),
                ("pages/page1.jpg", b"one"),
            ],
        );
        assert_eq!(read_cover(&path).as_deref(), Some(&b"one"[..]));
        // A zip under a .cbr name is opened by content.
        let mislabeled = dir.join("issue.cbr");
        std::fs::copy(&path, &mislabeled).unwrap();
        assert_eq!(read_cover(&mislabeled).as_deref(), Some(&b"one"[..]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn epub_cover_follows_the_package_document() {
        let dir = scratch("epub");
        let path = dir.join("book.epub");
        let container = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
        let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata><meta name="cover" content="img-front"/></metadata>
  <manifest>
    <item id="a" href="images/a%20first.png" media-type="image/png"/>
    <item id="img-front" href="images/front.jpg" media-type="image/jpeg"/>
  </manifest>
</package>"#;
        write_zip(
            &path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", container),
                ("OEBPS/content.opf", opf),
                ("OEBPS/images/a first.png", b"first"),
                ("OEBPS/images/front.jpg", b"front"),
            ],
        );
        assert_eq!(read_cover(&path).as_deref(), Some(&b"front"[..]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opf_cover_precedence() {
        let epub3 = r#"<package><manifest>
            <item id="cover" href="old.jpg"/>
            <item id="x" href="new.png" properties="nav cover-image"/>
        </manifest></package>"#;
        assert_eq!(opf_cover_href(epub3).as_deref(), Some("new.png"));
        let named = r#"<package><manifest>
            <item id="p1" href="p1.jpg"/><item id="p2" href="img/Cover.jpeg"/>
        </manifest></package>"#;
        assert_eq!(opf_cover_href(named).as_deref(), Some("img/Cover.jpeg"));
        let plain = r#"<package><manifest>
            <item id="t" href="text.xhtml"/><item id="p1" href="p1.gif"/>
        </manifest></package>"#;
        assert_eq!(opf_cover_href(plain).as_deref(), Some("p1.gif"));
        assert_eq!(opf_cover_href("<package/>"), None);
    }

    #[test]
    fn hrefs_resolve_against_the_package_directory() {
        assert_eq!(resolve_href("OEBPS/content.opf", "images/c.jpg"), "OEBPS/images/c.jpg");
        assert_eq!(resolve_href("OEBPS/pkg/content.opf", "../img/c%20d.jpg#x"), "OEBPS/img/c d.jpg");
        assert_eq!(resolve_href("content.opf", "./c.jpg"), "c.jpg");
    }
}
