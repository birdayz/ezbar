//! Bounded data-only icon decoding. SNI pixmaps use network-order ARGB; X11
//! images use the server's byte order, visual masks and padded scanlines.
use std::{
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_DIM: u32 = 512;
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    Pixels(Pixels),
}

fn pixel_bytes(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

pub fn argb(width: i32, height: i32, bytes: &[u8]) -> Option<Pixels> {
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if bytes.len() != pixel_bytes(width, height)? {
        return None;
    }
    let rgba = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[1], p[2], p[3], p[0]])
        .collect::<Vec<_>>();
    Some(Pixels {
        width,
        height,
        rgba: rgba.into(),
    })
}

pub fn best_pixmap(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<Icon> {
    // Prefer a usable source at least 24px; do not let an invalid "best" entry
    // hide a valid smaller one. Icons are bounded again before allocating.
    pixmaps
        .into_iter()
        .filter_map(|(w, h, data)| argb(w, h, &data))
        .min_by_key(|p| {
            if p.width >= 24 && p.height >= 24 {
                p.width.max(p.height)
            } else {
                1024 - p.width.min(p.height)
            }
        })
        .map(Icon::Pixels)
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE).then_some(bytes)
}

fn load(path: &Path) -> Option<Icon> {
    let bytes = read_bounded(path)?;
    if path.extension()?.to_str()? == "svg" {
        return svg_pixels(&bytes).map(Icon::Pixels);
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: 4 * 1024 * 1024,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    pixel_bytes(info.width, info.height)?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let input = &buffer[..frame.buffer_size()];
    let rgba = match frame.color_type {
        png::ColorType::Rgba => input.to_vec(),
        png::ColorType::Rgb => input
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => input.iter().flat_map(|p| [*p, *p, *p, 255]).collect(),
        png::ColorType::GrayscaleAlpha => input
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => return None,
    };
    Some(Icon::Pixels(Pixels {
        width: frame.width,
        height: frame.height,
        rgba: rgba.into(),
    }))
}

fn svg_pixels(bytes: &[u8]) -> Option<Pixels> {
    // Plain SVG only (no gzip expansion), small source and fixed raster target.
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(std::str::from_utf8(bytes).ok()?, &options).ok()?;
    let mut image = resvg::tiny_skia::Pixmap::new(48, 48)?;
    let scale = 48.0 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut image.as_mut(),
    );
    let rgba: Vec<u8> = image
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    Some(Pixels {
        width: 48,
        height: 48,
        rgba: rgba.into(),
    })
}

pub fn named(name: &str, theme_path: &str) -> Option<Icon> {
    if name.is_empty() || name.len() > 256 {
        return None;
    }
    if Path::new(name).is_absolute() {
        return load(Path::new(name));
    }
    // Icon names are not paths. Prevent directory traversal through an app's name.
    if name.contains('/') || name == "." || name == ".." {
        return None;
    }
    let mut roots = Vec::new();
    if !theme_path.is_empty() && Path::new(theme_path).is_absolute() {
        roots.push(PathBuf::from(theme_path));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".local/share/icons/hicolor"));
        roots.push(PathBuf::from(home).join(".icons/hicolor"));
    }
    roots.extend(
        [
            "/usr/share/icons/hicolor",
            "/usr/share/icons/Adwaita",
            "/usr/share/pixmaps",
        ]
        .map(PathBuf::from),
    );
    for root in roots {
        for suffix in [
            "",
            "24x24/status",
            "24x24/apps",
            "22x22/status",
            "22x22/apps",
            "32x32/status",
            "32x32/apps",
            "16x16/status",
            "16x16/apps",
            "scalable/status",
            "scalable/apps",
            "256x256/apps",
        ] {
            for ext in ["png", "svg"] {
                let filename = if name.ends_with(&format!(".{ext}")) {
                    name.to_owned()
                } else {
                    format!("{name}.{ext}")
                };
                if let Some(icon) = load(&root.join(suffix).join(filename)) {
                    return Some(icon);
                }
            }
        }
    }
    None
}

/// TrueColor pixels (16/24/30/32-bit depths). Reject truncated/oversized buffers
/// and unsupported bit depths instead of guessing at layout or indexing past it.
#[allow(clippy::too_many_arguments)]
pub fn ximage(
    width: u32,
    height: u32,
    bpp: u8,
    pad: u8,
    depth: u8,
    little_endian: bool,
    masks: [u32; 3],
    bytes: &[u8],
) -> Option<Pixels> {
    let length = pixel_bytes(width, height)?;
    if !matches!(bpp, 16 | 24 | 32) || !matches!(pad, 8 | 16 | 32) {
        return None;
    }
    let stride = (width as usize * bpp as usize).div_ceil(pad as usize) * (pad as usize / 8);
    if bytes.len() < stride.checked_mul(height as usize)? {
        return None;
    }
    let channel = |pixel: u32, mask: u32| -> u8 {
        if mask == 0 {
            return 0;
        }
        (((pixel & mask) >> mask.trailing_zeros()) as u64 * 255
            / (mask >> mask.trailing_zeros()) as u64) as u8
    };
    let alpha = if depth == 32 {
        !(masks[0] | masks[1] | masks[2])
    } else {
        0
    };
    let mut rgba = Vec::with_capacity(length);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let start = y * stride + x * (bpp as usize / 8);
            let source = &bytes[start..start + bpp as usize / 8];
            let mut value = 0u32;
            for (i, byte) in source.iter().enumerate() {
                let shift = if little_endian {
                    i
                } else {
                    source.len() - 1 - i
                };
                value |= u32::from(*byte) << (8 * shift);
            }
            let a = if alpha == 0 {
                255
            } else {
                channel(value, alpha)
            };
            for mask in masks {
                let c = channel(value, mask);
                rgba.push(if alpha != 0 && a > 0 {
                    (u32::from(c) * 255 / u32::from(a)).min(255) as u8
                } else {
                    c
                });
            }
            rgba.push(a);
        }
    }
    Some(Pixels {
        width,
        height,
        rgba: rgba.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sni_byte_order_and_bounds() {
        assert_eq!(
            &*argb(1, 1, &[128, 20, 30, 40]).unwrap().rgba,
            &[20, 30, 40, 128]
        );
        for (w, h) in [(0, 1), (-1, 1), (513, 1), (i32::MAX, i32::MAX)] {
            assert!(argb(w, h, &[]).is_none());
        }
        assert!(argb(1, 1, &[0; 3]).is_none());
    }
    #[test]
    fn invalid_candidate_does_not_hide_valid_icon() {
        assert!(best_pixmap(vec![(24, 24, vec![]), (1, 1, vec![255; 4])]).is_some());
    }
    #[test]
    fn x11_padding_endian_and_alpha() {
        let masks = [0xff0000, 0xff00, 0xff];
        let p = ximage(1, 2, 24, 32, 24, true, masks, &[3, 2, 1, 0, 6, 5, 4, 0]).unwrap();
        assert_eq!(&*p.rgba, &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(
            &*ximage(1, 1, 32, 32, 32, false, masks, &[128, 64, 32, 0])
                .unwrap()
                .rgba,
            &[127, 63, 0, 128]
        );
        assert!(ximage(2, 2, 32, 32, 32, true, masks, &[0; 4]).is_none());
    }
    #[test]
    fn svg_has_fixed_output_and_no_external_images() {
        let p = svg_pixels(br##"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"><rect width="100000" height="100000" fill="#ff0000"/></svg>"##).unwrap();
        assert_eq!((p.width, p.height, p.rgba.len()), (48, 48, 48 * 48 * 4));
        assert_eq!(&p.rgba[..4], &[255, 0, 0, 255]);
        let p = svg_pixels(br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><image href="/dev/zero" width="24" height="24"/></svg>"#).unwrap();
        assert!(p.rgba.iter().all(|v| *v == 0));
        assert!(svg_pixels(&vec![b' '; 65537]).is_none());
        assert!(svg_pixels(&[0x1f, 0x8b, 0, 0]).is_none());
    }

    #[test]
    fn icon_name_is_not_a_relative_path() {
        assert!(named("../../etc/passwd", "").is_none());
        assert!(named("", "").is_none());
    }
}
