//! The application icon: the PNG images in `assets/icons/png` (rasterized from the SVG sources
//! in `assets/icons`: `icon-16.svg` for 16 and 20 px, `icon.svg` from 24 px) and what is built from them. `cargo xtask icons` assembles the Windows
//! icon `assets/icons/vibelauncher.ico` (PNG images in an ICO container), which the build scripts
//! and the MSI embed; the Linux packages take the hicolor images and the macOS bundle its ICNS
//! file from the same PNG files.

use std::path::PathBuf;

use crate::package::read;
use crate::root;

/// The sizes in the Windows icon.
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 48, 64, 128, 256];

/// The folder with the PNG images, named `<size>.png`.
fn png_dir() -> PathBuf {
    root().join("assets").join("icons").join("png")
}

/// The PNG image of `size` pixels, its header checked.
pub(crate) fn png(size: u32) -> Result<Vec<u8>, String> {
    let path = png_dir().join(format!("{size}.png"));
    let bytes = read(&path)?;
    let dim = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") {
        return Err(format!("{}: not a PNG file", path.display()));
    }
    if dim(16) != Some(size) || dim(20) != Some(size) {
        return Err(format!("{}: not {size}x{size} pixels", path.display()));
    }
    Ok(bytes)
}

/// `(size, PNG)` of each of `sizes`.
pub(crate) fn pngs(sizes: &[u32]) -> Result<Vec<(u32, Vec<u8>)>, String> {
    sizes.iter().map(|&s| Ok((s, png(s)?))).collect()
}

/// An ICO file of the PNG images `images` (`(size, PNG)`, at most 256 px).
fn ico(images: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(images.len()).map_err(|_| "too many images")?;
    let mut out = vec![0, 0, 1, 0];
    out.extend(count.to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    let mut data: Vec<u8> = Vec::new();
    for (size, png) in images {
        // A size of 256 is stored as 0.
        let dim = u8::try_from(*size % 256).map_err(|_| "icon too large")?;
        if *size > 256 {
            return Err(format!("{size} px does not fit an ICO entry"));
        }
        out.extend([dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend(
            u32::try_from(png.len())
                .map_err(|_| "icon too large")?
                .to_le_bytes(),
        );
        out.extend(
            u32::try_from(offset)
                .map_err(|_| "icon too large")?
                .to_le_bytes(),
        );
        offset += png.len();
        data.extend(png);
    }
    out.extend(data);
    Ok(out)
}

/// The Windows icon built from the committed PNG images.
fn windows_icon() -> Result<Vec<u8>, String> {
    ico(&pngs(&ICO_SIZES)?)
}

/// `cargo xtask icons`: writes `assets/icons/vibelauncher.ico`.
pub fn generate() -> Result<(), String> {
    let path = root().join("assets").join("icons").join("vibelauncher.ico");
    std::fs::write(&path, windows_icon()?).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ico_directory_lists_the_images() {
        let ico = ico(&[(16, b"a".to_vec()), (256, b"bb".to_vec())]).unwrap();
        assert_eq!(&ico[..6], [0, 0, 1, 0, 2, 0]);
        assert_eq!(ico[6], 16);
        assert_eq!(ico[22], 0);
        assert_eq!(u32::from_le_bytes(ico[14..18].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(ico[18..22].try_into().unwrap()), 38);
        assert_eq!(u32::from_le_bytes(ico[34..38].try_into().unwrap()), 39);
        assert_eq!(&ico[38..], b"abb");
        assert!(super::ico(&[(512, b"x".to_vec())]).is_err());
    }

    #[test]
    fn the_committed_icon_is_built_from_the_committed_images() {
        let committed = read(&root().join("assets/icons/vibelauncher.ico")).unwrap();
        assert!(
            committed == windows_icon().unwrap(),
            "assets/icons/vibelauncher.ico is stale: run `cargo xtask icons`"
        );
    }

    #[test]
    fn every_size_has_an_image() {
        for size in [16, 20, 24, 32, 48, 64, 128, 256, 512] {
            png(size).unwrap();
        }
    }
}
