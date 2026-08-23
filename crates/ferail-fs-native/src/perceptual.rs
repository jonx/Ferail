//! In-memory perceptual image indexing for Similar Images.
//!
//! Nothing in this module persists paths, pixels, thumbnails, or hashes and it
//! performs no network I/O. A decoded full-resolution image is reduced to two
//! 64-bit hashes, a few scalar facts, and a small RGBA thumbnail before the
//! source pixels are dropped.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView as _, ImageDecoder as _, ImageReader};

/// Algorithm revision for tests and future tuning. This is deliberately not a
/// database revision: Similar Images has no persistent analysis cache.
pub const PERCEPTUAL_REVISION: u32 = 1;
pub const DHASH_MAX_DISTANCE: u32 = 7;
pub const PHASH_MAX_DISTANCE: u32 = 12;

/// User-adjustable matching limits. Values are capped at the widest ranges
/// for which the current banded candidate search is exact and tuned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimilarityCriteria {
    pub structure: u32,
    pub detail: u32,
}

impl SimilarityCriteria {
    pub const RECOMMENDED: Self = Self {
        structure: DHASH_MAX_DISTANCE,
        detail: PHASH_MAX_DISTANCE,
    };

    pub fn clamped(self) -> Self {
        Self {
            structure: self.structure.min(DHASH_MAX_DISTANCE),
            detail: self.detail.min(PHASH_MAX_DISTANCE),
        }
    }
}

impl Default for SimilarityCriteria {
    fn default() -> Self {
        Self::RECOMMENDED
    }
}
/// Euclidean distance in mean RGB space. Conservative enough to reject the
/// classic all-flat dHash/pHash collision without excluding normal JPEG drift.
pub const MEAN_RGB_MAX_DISTANCE: u32 = 72;
/// Flat images have almost no perceptual structure, so hashes collapse into
/// enormous, unsafe groups. Exact duplicates still belong in Exact mode.
pub const MIN_LOW_INFORMATION_VARIANCE: u16 = 16;
/// Images smaller than this in either dimension are predominantly icons and
/// UI chrome, where perceptual hashes produce unhelpful giant groups.
pub const MIN_IMAGE_DIMENSION: u32 = 32;
/// Bound hostile/decompression-bomb inputs before allocating decoded pixels.
const MAX_IMAGE_DIMENSION: u32 = 50_000;
const MAX_IMAGE_PIXELS: u64 = 200_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerceptualSignature {
    pub dhash: u64,
    pub phash: u64,
    pub mean_rgb: [u8; 3],
    /// Mean squared deviation of normalized luminance samples.
    pub luma_variance: u16,
    pub width: u32,
    pub height: u32,
}

impl PerceptualSignature {
    pub fn pixel_area(self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerceptualThumbnail {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedImage {
    pub signature: PerceptualSignature,
    pub thumbnail: PerceptualThumbnail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimilarityPair {
    pub a: usize,
    pub b: usize,
    pub dhash_distance: u32,
    pub phash_distance: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimilarityCluster {
    /// A real member (a medoid), never an averaged synthetic hash.
    pub medoid: usize,
    pub members: Vec<usize>,
}

pub fn is_supported_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff")
    )
}

/// Decode, orient, index, and thumbnail one local image. Errors are intentionally
/// path-free so callers can aggregate counts without leaking private filenames
/// into logs or diagnostics.
pub fn index_path(path: &Path, thumbnail_px: u32) -> Result<IndexedImage, &'static str> {
    let image = decode_oriented(path)?;
    Ok(index_image(&image, thumbnail_px.max(1)))
}

/// Decode only long enough to compute the compact signature. The source pixels
/// are dropped on return and no thumbnail is retained for non-result images.
pub fn signature_path(path: &Path) -> Result<PerceptualSignature, &'static str> {
    let image = decode_oriented(path)?;
    Ok(signature(&image))
}

fn decode_oriented(path: &Path) -> Result<DynamicImage, &'static str> {
    let mut reader = ImageReader::open(path).map_err(|_| "open failed")?;
    reader = reader
        .with_guessed_format()
        .map_err(|_| "format detection failed")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| "unsupported image")?;
    let (width, height) = decoder.dimensions();
    if width < MIN_IMAGE_DIMENSION || height < MIN_IMAGE_DIMENSION {
        return Err("image too small");
    }
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS {
        return Err("image too large");
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| "decode failed")?;
    image.apply_orientation(orientation);
    Ok(image)
}

pub fn index_image(image: &DynamicImage, thumbnail_px: u32) -> IndexedImage {
    let signature = signature(image);
    let thumb = image.thumbnail(thumbnail_px, thumbnail_px).to_rgba8();
    IndexedImage {
        signature,
        thumbnail: PerceptualThumbnail {
            width: thumb.width(),
            height: thumb.height(),
            rgba: thumb.into_raw(),
        },
    }
}

pub fn signature(image: &DynamicImage) -> PerceptualSignature {
    let (width, height) = image.dimensions();
    let normalized = normalized_rgb(image, 32, 32);
    let gray: Vec<f32> = normalized.iter().map(|rgb| luminance(*rgb)).collect();

    let sums = normalized.iter().fold([0u64; 3], |mut sums, rgb| {
        sums[0] += u64::from(rgb[0]);
        sums[1] += u64::from(rgb[1]);
        sums[2] += u64::from(rgb[2]);
        sums
    });
    let count = normalized.len() as u64;
    let mean_rgb = [
        (sums[0] / count) as u8,
        (sums[1] / count) as u8,
        (sums[2] / count) as u8,
    ];
    let mean_luma = gray.iter().copied().sum::<f32>() / gray.len() as f32;
    let variance = gray
        .iter()
        .map(|value| {
            let delta = *value - mean_luma;
            delta * delta
        })
        .sum::<f32>()
        / gray.len() as f32;

    PerceptualSignature {
        dhash: dhash(image),
        phash: phash_from_gray(&gray),
        mean_rgb,
        luma_variance: variance.round().clamp(0.0, u16::MAX as f32) as u16,
        width,
        height,
    }
}

fn normalized_rgb(image: &DynamicImage, width: u32, height: u32) -> Vec<[u8; 3]> {
    image
        .resize_exact(width, height, FilterType::Triangle)
        .to_rgba8()
        .pixels()
        .map(|pixel| {
            let [r, g, b, a] = pixel.0;
            // Composite transparency over white. Hidden RGB in a transparent
            // PNG must not influence what a person perceives.
            let composite = |channel: u8| {
                ((u16::from(channel) * u16::from(a) + 255 * u16::from(255u8.saturating_sub(a)))
                    / 255) as u8
            };
            [composite(r), composite(g), composite(b)]
        })
        .collect()
}

fn luminance(rgb: [u8; 3]) -> f32 {
    0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2])
}

fn dhash(image: &DynamicImage) -> u64 {
    let pixels = normalized_rgb(image, 9, 8);
    let mut hash = 0u64;
    let mut bit = 0u32;
    for y in 0..8usize {
        for x in 0..8usize {
            let left = luminance(pixels[y * 9 + x]);
            let right = luminance(pixels[y * 9 + x + 1]);
            if left > right {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    hash
}

fn phash_from_gray(gray: &[f32]) -> u64 {
    debug_assert_eq!(gray.len(), 32 * 32);
    let mut cosines = [[0.0f32; 32]; 8];
    for (frequency, row) in cosines.iter_mut().enumerate() {
        for (position, value) in row.iter_mut().enumerate() {
            *value =
                ((std::f32::consts::PI / 32.0) * (position as f32 + 0.5) * frequency as f32).cos();
        }
    }
    let mut coefficients = [0.0f32; 64];
    for v in 0..8usize {
        for u in 0..8usize {
            let mut sum = 0.0f32;
            for y in 0..32usize {
                for x in 0..32usize {
                    sum += gray[y * 32 + x] * cosines[u][x] * cosines[v][y];
                }
            }
            coefficients[v * 8 + u] = sum;
        }
    }
    let mut ac = coefficients[1..].to_vec();
    ac.sort_by(f32::total_cmp);
    let median = ac[ac.len() / 2];
    coefficients
        .iter()
        .enumerate()
        .fold(0u64, |hash, (bit, value)| {
            hash | (u64::from(*value > median) << bit)
        })
}

pub fn hash_distances(a: PerceptualSignature, b: PerceptualSignature) -> (u32, u32) {
    (
        (a.dhash ^ b.dhash).count_ones(),
        (a.phash ^ b.phash).count_ones(),
    )
}

fn mean_rgb_distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    let squared = a
        .into_iter()
        .zip(b)
        .map(|(a, b)| {
            let delta = i32::from(a) - i32::from(b);
            (delta * delta) as u32
        })
        .sum::<u32>();
    (squared as f64).sqrt().round() as u32
}

pub fn are_similar(a: PerceptualSignature, b: PerceptualSignature) -> bool {
    are_similar_with(a, b, SimilarityCriteria::RECOMMENDED)
}

pub fn are_similar_with(
    a: PerceptualSignature,
    b: PerceptualSignature,
    criteria: SimilarityCriteria,
) -> bool {
    if a.luma_variance < MIN_LOW_INFORMATION_VARIANCE
        && b.luma_variance < MIN_LOW_INFORMATION_VARIANCE
    {
        return false;
    }
    let criteria = criteria.clamped();
    let (dhash, phash) = hash_distances(a, b);
    dhash <= criteria.structure
        && phash <= criteria.detail
        && mean_rgb_distance(a.mean_rgb, b.mean_rgb) <= MEAN_RGB_MAX_DISTANCE
}

/// Candidate edges using eight dHash bands. Any pair with dHash Hamming
/// distance ≤ 7 must share at least one unchanged band, so this loses no pair
/// accepted by [`are_similar`]. Degenerate buckets can still be quadratic;
/// pHash and mean RGB reject their false edges before grouping.
pub fn similarity_pairs(signatures: &[PerceptualSignature]) -> Vec<SimilarityPair> {
    similarity_pairs_with(signatures, SimilarityCriteria::RECOMMENDED)
}

pub fn similarity_pairs_with(
    signatures: &[PerceptualSignature],
    criteria: SimilarityCriteria,
) -> Vec<SimilarityPair> {
    let criteria = criteria.clamped();
    let mut buckets: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
    for (index, signature) in signatures.iter().enumerate() {
        for band in 0..8u8 {
            let value = ((signature.dhash >> (u32::from(band) * 8)) & 0xff) as u8;
            buckets.entry((band, value)).or_default().push(index);
        }
    }
    let mut candidates = BTreeSet::new();
    for bucket in buckets.values() {
        for left in 0..bucket.len() {
            for right in left + 1..bucket.len() {
                candidates.insert((
                    bucket[left].min(bucket[right]),
                    bucket[left].max(bucket[right]),
                ));
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(a, b)| {
            let sa = signatures[a];
            let sb = signatures[b];
            if !are_similar_with(sa, sb, criteria) {
                return None;
            }
            let (dhash_distance, phash_distance) = hash_distances(sa, sb);
            Some(SimilarityPair {
                a,
                b,
                dhash_distance,
                phash_distance,
            })
        })
        .collect()
}

/// Form connected components, then repeatedly peel a medoid-centred star from
/// each component. Every emitted member is within both thresholds of its
/// group's medoid, preventing transitive A≈B≈C chains from drifting unchecked.
pub fn similarity_clusters(signatures: &[PerceptualSignature]) -> Vec<SimilarityCluster> {
    similarity_clusters_with(signatures, SimilarityCriteria::RECOMMENDED)
}

pub fn similarity_clusters_with(
    signatures: &[PerceptualSignature],
    criteria: SimilarityCriteria,
) -> Vec<SimilarityCluster> {
    let criteria = criteria.clamped();
    let pairs = similarity_pairs_with(signatures, criteria);
    let mut adjacency = vec![Vec::<usize>::new(); signatures.len()];
    for pair in &pairs {
        adjacency[pair.a].push(pair.b);
        adjacency[pair.b].push(pair.a);
    }
    let mut seen = vec![false; signatures.len()];
    let mut out = Vec::new();
    for start in 0..signatures.len() {
        if seen[start] || adjacency[start].is_empty() {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut component = Vec::new();
        seen[start] = true;
        while let Some(index) = queue.pop_front() {
            component.push(index);
            for &next in &adjacency[index] {
                if !seen[next] {
                    seen[next] = true;
                    queue.push_back(next);
                }
            }
        }
        let mut remaining: BTreeSet<usize> = component.into_iter().collect();
        while remaining.len() >= 2 {
            let medoid = *remaining
                .iter()
                .min_by_key(|&&candidate| {
                    remaining
                        .iter()
                        .map(|&other| {
                            let (d, p) = hash_distances(signatures[candidate], signatures[other]);
                            u64::from(d) + u64::from(p)
                        })
                        .sum::<u64>()
                })
                .expect("remaining is non-empty");
            let members: Vec<usize> = remaining
                .iter()
                .copied()
                .filter(|&index| {
                    index == medoid
                        || are_similar_with(signatures[medoid], signatures[index], criteria)
                })
                .collect();
            if members.len() < 2 {
                remaining.remove(&medoid);
                continue;
            }
            for member in &members {
                remaining.remove(member);
            }
            out.push(SimilarityCluster { medoid, members });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn synthetic(width: u32, height: u32, phase: u8) -> DynamicImage {
        DynamicImage::ImageRgba8(ImageBuffer::from_fn(width, height, |x, y| {
            let wave = (((x / 12 + y / 17) % 2) * 52) as u8;
            Rgba([
                (x as u8).wrapping_add(wave).wrapping_add(phase),
                (y as u8).wrapping_mul(2).wrapping_add(phase / 2),
                (x as u8 ^ y as u8).wrapping_add(wave),
                255,
            ])
        }))
    }

    #[test]
    fn resized_copy_stays_similar_and_keeps_original_dimensions() {
        let original = synthetic(640, 480, 0);
        let resized = original.resize_exact(160, 120, FilterType::Lanczos3);
        let a = signature(&original);
        let b = signature(&resized);
        assert!(are_similar(a, b), "distances: {:?}", hash_distances(a, b));
        assert_eq!((a.width, a.height), (640, 480));
        assert_eq!((b.width, b.height), (160, 120));
    }

    #[test]
    fn jpeg_recompression_stays_similar() {
        use image::codecs::jpeg::JpegEncoder;
        use std::io::Cursor;

        let original = synthetic(480, 320, 0);
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 58)
            .encode_image(&original)
            .unwrap();
        let recompressed = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        let a = signature(&original);
        let b = signature(&recompressed);
        assert!(are_similar(a, b), "distances: {:?}", hash_distances(a, b));
    }

    #[test]
    fn modest_crop_stays_similar() {
        let original = synthetic(640, 480, 0);
        let cropped =
            original
                .crop_imm(16, 12, 608, 456)
                .resize_exact(640, 480, FilterType::Lanczos3);
        let a = signature(&original);
        let b = signature(&cropped);
        assert!(are_similar(a, b), "distances: {:?}", hash_distances(a, b));
    }

    #[test]
    fn unrelated_images_do_not_match() {
        let a = signature(&synthetic(320, 240, 0));
        let b = signature(&DynamicImage::ImageRgba8(ImageBuffer::from_fn(
            320,
            240,
            |x, y| Rgba([255 - x as u8, 20, 255 - y as u8, 255]),
        )));
        assert!(!are_similar(a, b));
    }

    #[test]
    fn near_blank_images_do_not_form_a_perceptual_group() {
        let flat =
            |value| DynamicImage::ImageRgba8(ImageBuffer::from_pixel(320, 240, Rgba([value; 4])));
        let a = signature(&flat(245));
        let b = signature(&flat(246));
        assert_eq!(hash_distances(a, b).0, 0);
        assert_eq!((a.luma_variance, b.luma_variance), (0, 0));
        assert!(!are_similar(a, b));
    }

    #[test]
    fn banding_finds_every_pair_with_seven_or_fewer_dhash_bit_flips() {
        let base = PerceptualSignature {
            dhash: 0x1234_5678_90ab_cdef,
            phash: 0x0f0f_f0f0_55aa_a55a,
            mean_rgb: [100, 110, 120],
            luma_variance: 100,
            width: 100,
            height: 100,
        };
        for flips in 0..=7u32 {
            let changed = PerceptualSignature {
                dhash: base.dhash ^ ((1u64 << flips).saturating_sub(1)),
                ..base
            };
            assert_eq!(similarity_pairs(&[base, changed]).len(), 1, "flips={flips}");
        }
    }

    #[test]
    fn adjustable_criteria_can_tighten_structure_and_detail_independently() {
        let base = PerceptualSignature {
            dhash: 0,
            phash: 0,
            mean_rgb: [100; 3],
            luma_variance: 100,
            width: 100,
            height: 100,
        };
        let changed = PerceptualSignature {
            dhash: 0b111,
            phash: 0b1_1111,
            ..base
        };
        assert!(are_similar_with(
            base,
            changed,
            SimilarityCriteria {
                structure: 3,
                detail: 5,
            }
        ));
        assert!(!are_similar_with(
            base,
            changed,
            SimilarityCriteria {
                structure: 2,
                detail: 5,
            }
        ));
        assert!(!are_similar_with(
            base,
            changed,
            SimilarityCriteria {
                structure: 3,
                detail: 4,
            }
        ));
    }

    #[test]
    fn transitive_chain_is_split_into_medoid_bounded_groups() {
        let sig = |dhash| PerceptualSignature {
            dhash,
            phash: dhash,
            mean_rgb: [100; 3],
            luma_variance: 100,
            width: 100,
            height: 100,
        };
        // Consecutive members are close, endpoints are not.
        let signatures = [sig(0), sig(0x3f), sig(0xfc0), sig(0x3f000)];
        let clusters = similarity_clusters(&signatures);
        assert!(!clusters.is_empty());
        for cluster in clusters {
            assert!(cluster.members.len() >= 2);
            assert!(cluster
                .members
                .iter()
                .all(|&member| are_similar(signatures[cluster.medoid], signatures[member])));
        }
    }

    #[test]
    fn indexing_produces_a_bounded_memory_thumbnail() {
        let indexed = index_image(&synthetic(640, 480, 0), 96);
        assert_eq!(
            (indexed.thumbnail.width, indexed.thumbnail.height),
            (96, 72)
        );
        assert_eq!(indexed.thumbnail.rgba.len(), 96 * 72 * 4);
    }
}
