use crate::process;
use anyhow::{Result, bail};
use flate2::{Compression, write::ZlibEncoder};
use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};
use std::{fs, io::Write, path::Path};

pub const SUPPORTED: &[&str] = &[
    ".pdf", ".txt", ".md", ".doc", ".docx", ".odt", ".rtf", ".xls", ".xlsx", ".ods", ".csv",
    ".ppt", ".pptx", ".odp", ".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff", ".bmp",
];
const IMAGES: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff", ".bmp",
];
const OUTPUT_LIMIT: usize = 100 * 1024 * 1024;
const PROFILE: &str = r#"<?xml version="1.0"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting">
<prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop>
<prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security">
<prop oor:name="BlockUntrustedRefererLinks" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Calc/Content/Update">
<prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
</oor:items>"#;

fn decode(path: &Path) -> Result<DynamicImage> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(160 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 40_000_000 {
        bail!("This image is too large to process safely.");
    }
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

struct Document {
    pdf: Pdf,
    pages: Vec<Ref>,
    paper: (f32, f32),
    bytes: usize,
}
impl Document {
    fn new(paper: &str) -> Self {
        let mut pdf = Pdf::new();
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        Self {
            pdf,
            pages: vec![],
            paper: if paper == "A4" {
                (595.28, 841.89)
            } else {
                (612.0, 792.0)
            },
            bytes: 0,
        }
    }
    fn image(&mut self, image: DynamicImage) -> Result<()> {
        let image = if image.width().max(image.height()) > 2400 {
            image.resize(2400, 2400, FilterType::Lanczos3)
        } else {
            image
        };
        let (width, height) = (image.width(), image.height());
        // Flatten alpha onto white paper and discard all source metadata.
        let rgba = image.into_rgba8();
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in rgba.pixels() {
            let alpha = u32::from(pixel[3]);
            for channel in &pixel.0[..3] {
                rgb.push(((u32::from(*channel) * alpha + 255 * (255 - alpha)) / 255) as u8);
            }
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&rgb)?;
        let encoded = encoder.finish()?;
        self.bytes += encoded.len();
        if self.bytes > OUTPUT_LIMIT - 1024 * 1024 {
            bail!("The converted document is too large.");
        }
        let page_id = Ref::new(3 + self.pages.len() as i32 * 3);
        let image_id = Ref::new(page_id.get() + 1);
        let stream_id = Ref::new(page_id.get() + 2);
        self.pages.push(page_id);
        let name = Name(b"PageImage");
        let mut page = self.pdf.page(page_id);
        page.parent(Ref::new(2))
            .media_box(Rect::new(0.0, 0.0, self.paper.0, self.paper.1))
            .contents(stream_id);
        page.resources().x_objects().pair(name, image_id);
        page.finish();
        let mut object = self.pdf.image_xobject(image_id, &encoded);
        object.filter(Filter::FlateDecode);
        object
            .width(width as i32)
            .height(height as i32)
            .bits_per_component(8);
        object.color_space().device_rgb();
        object.finish();
        let scale =
            ((self.paper.0 - 48.0) / width as f32).min((self.paper.1 - 48.0) / height as f32);
        let (w, h) = (width as f32 * scale, height as f32 * scale);
        let mut content = Content::new();
        content
            .save_state()
            .transform([
                w,
                0.0,
                0.0,
                h,
                (self.paper.0 - w) / 2.0,
                (self.paper.1 - h) / 2.0,
            ])
            .x_object(name)
            .restore_state();
        self.pdf.stream(stream_id, &content.finish());
        Ok(())
    }
    fn finish(mut self, output: &Path) -> Result<usize> {
        let count = self.pages.len();
        self.pdf
            .pages(Ref::new(2))
            .kids(self.pages)
            .count(count as i32);
        let bytes = self.pdf.finish();
        if bytes.len() > OUTPUT_LIMIT {
            bail!("The converted document is too large.");
        }
        fs::write(output, bytes)?;
        Ok(count)
    }
}

/// Rebuild all output from pixels: no scripts, links, annotations, or embedded files survive.
pub async fn convert(source: &Path, output: &Path, paper: &str, max_pages: usize) -> Result<usize> {
    let suffix = format!(
        ".{}",
        source
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_lowercase()
    );
    if !SUPPORTED.contains(&suffix.as_str())
        || !["Letter", "A4"].contains(&paper)
        || !(1..=100).contains(&max_pages)
    {
        bail!("Invalid conversion options.");
    }
    let work = tempfile::tempdir()?;
    let mut document = Document::new(paper);
    if IMAGES.contains(&suffix.as_str()) {
        // GIF is a still image, matching Paperboy's existing first-frame behavior.
        // TIFF frames are extracted by the native renderer below when multipage.
        if [".tif", ".tiff"].contains(&suffix.as_str()) {
            return convert_tiff(source, output, paper, max_pages);
        }
        document.image(decode(source)?)?;
        return document.finish(output);
    }
    if [".txt", ".md"].contains(&suffix.as_str()) {
        return convert_text(source, output, paper, max_pages);
    }
    let mut pdf_source = source.to_path_buf();
    if suffix != ".pdf" {
        let profile = work.path().join("profile");
        fs::create_dir_all(profile.join("user"))?;
        fs::write(profile.join("user/registrymodifications.xcu"), PROFILE)?;
        let input = source.to_path_buf();
        process::run(
            "libreoffice",
            &[
                format!("-env:UserInstallation=file://{}", profile.display()),
                "--headless".into(),
                "--nologo".into(),
                "--nodefault".into(),
                "--norestore".into(),
                "--convert-to".into(),
                "pdf".into(),
                "--outdir".into(),
                work.path().display().to_string(),
                input.display().to_string(),
            ],
            90,
        )
        .await?;
        pdf_source = work.path().join(format!(
            "{}.pdf",
            input.file_stem().unwrap_or_default().to_string_lossy()
        ));
        if !pdf_source.is_file() {
            bail!("This document could not be converted. Try exporting it as PDF.");
        }
    }
    let info = process::run(
        "pdfinfo",
        &[
            "-box".into(),
            "-f".into(),
            "1".into(),
            "-l".into(),
            max_pages.to_string(),
            pdf_source.display().to_string(),
        ],
        15,
    )
    .await
    .map_err(|_| anyhow::anyhow!("This PDF could not be opened. Send an unlocked copy."))?;
    let info = String::from_utf8_lossy(&info);
    let pages: usize = info
        .lines()
        .find_map(|line| {
            line.strip_prefix("Pages:")
                .and_then(|v| v.trim().parse().ok())
        })
        .ok_or_else(|| anyhow::anyhow!("This PDF has no printable pages."))?;
    if info
        .lines()
        .any(|line| line.starts_with("Encrypted:") && line.contains("yes"))
    {
        bail!("This PDF is password protected. Send an unlocked copy.");
    }
    if !(1..=max_pages).contains(&pages) {
        bail!("This file must contain 1–{max_pages} pages.");
    }
    for line in info
        .lines()
        .filter(|line| line.starts_with("Page") && line.contains("size:"))
    {
        for value in line
            .split_once("size:")
            .unwrap()
            .1
            .split_whitespace()
            .filter_map(|v| v.parse::<f64>().ok())
        {
            if !value.is_finite() || value > 14400.0 {
                bail!("This document has an unusually large page.");
            }
        }
    }
    for page in 1..=pages {
        let prefix = work.path().join("page");
        process::run(
            "pdftoppm",
            &[
                "-f".into(),
                page.to_string(),
                "-l".into(),
                page.to_string(),
                "-singlefile".into(),
                "-r".into(),
                "150".into(),
                "-scale-to".into(),
                "2400".into(),
                "-png".into(),
                pdf_source.display().to_string(),
                prefix.display().to_string(),
            ],
            30,
        )
        .await?;
        document.image(decode(&prefix.with_extension("png"))?)?;
        fs::remove_file(prefix.with_extension("png"))?;
    }
    document.finish(output)
}

fn convert_text(source: &Path, output: &Path, paper: &str, max_pages: usize) -> Result<usize> {
    use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
    let text = fs::read_to_string(source)?;
    let font_path = std::env::var("PAPERBOY_FONT")
        .unwrap_or_else(|_| "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into());
    let font = FontArc::try_from_vec(fs::read(font_path)?)?;
    let scaled = font.as_scaled(PxScale::from(24.0));
    let (width, height) = if paper == "A4" {
        (1240, 1754)
    } else {
        (1275, 1650)
    };
    let mut page = image::RgbImage::from_pixel(width, height, image::Rgb([255; 3]));
    let mut document = Document::new(paper);
    let mut y = 100.0;
    let mut drew = false;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.replace('\t', "    ");
        let mut chars = line.chars().peekable();
        loop {
            if y > height as f32 - 100.0 {
                document.image(DynamicImage::ImageRgb8(page))?;
                if document.pages.len() >= max_pages {
                    bail!("This file exceeds the {max_pages}-page limit.");
                }
                page = image::RgbImage::from_pixel(width, height, image::Rgb([255; 3]));
                y = 100.0;
            }
            let mut x = 87.0;
            while let Some(&character) = chars.peek() {
                let mut glyph = scaled.scaled_glyph(character);
                let advance = scaled.h_advance(glyph.id);
                if x + advance > width as f32 - 87.0 {
                    break;
                }
                chars.next();
                glyph.position = point(x, y);
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    outlined.draw(|gx, gy, coverage| {
                        let px = bounds.min.x as i32 + gx as i32;
                        let py = bounds.min.y as i32 + gy as i32;
                        if px >= 0 && py >= 0 && px < width as i32 && py < height as i32 {
                            let pixel = page.get_pixel_mut(px as u32, py as u32);
                            for channel in &mut pixel.0 {
                                *channel = (f32::from(*channel) * (1.0 - coverage)) as u8;
                            }
                        }
                    });
                }
                x += advance;
            }
            y += 29.0;
            drew = true;
            if chars.peek().is_none() {
                break;
            }
        }
    }
    if drew || document.pages.is_empty() {
        document.image(DynamicImage::ImageRgb8(page))?;
    }
    document.finish(output)
}

fn convert_tiff(source: &Path, output: &Path, paper: &str, max_pages: usize) -> Result<usize> {
    use tiff::{
        ColorType,
        decoder::{Decoder, DecodingResult, Limits},
    };
    let mut limits = Limits::default();
    limits.decoding_buffer_size = 160 * 1024 * 1024;
    limits.intermediate_buffer_size = 64 * 1024 * 1024;
    let mut decoder =
        Decoder::new(std::io::BufReader::new(fs::File::open(source)?))?.with_limits(limits);
    let mut document = Document::new(paper);
    loop {
        if document.pages.len() >= max_pages {
            bail!("This file exceeds the {max_pages}-page limit.");
        }
        let (width, height) = decoder.dimensions()?;
        if u64::from(width) * u64::from(height) > 40_000_000 {
            bail!("This image is too large to process safely.");
        }
        let color = decoder.colortype()?;
        if decoder
            .get_tag_unsigned::<u16>(tiff::tags::Tag::PlanarConfiguration)
            .unwrap_or(1)
            != 1
        {
            bail!("This TIFF layout is unsupported. Export it as PDF.");
        }
        let image = match (color, decoder.read_image()?) {
            (ColorType::Gray(1), DecodingResult::U8(data)) => {
                let stride = width.div_ceil(8) as usize;
                if data.len() != stride * height as usize {
                    bail!("Invalid TIFF image.");
                }
                let pixels = data
                    .chunks_exact(stride)
                    .flat_map(|row| {
                        (0..width as usize).map(move |x| {
                            if row[x / 8] & (128 >> (x % 8)) != 0 {
                                255
                            } else {
                                0
                            }
                        })
                    })
                    .collect();
                DynamicImage::ImageLuma8(
                    image::GrayImage::from_raw(width, height, pixels)
                        .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
                )
            }
            (ColorType::RGB(8), DecodingResult::U8(data)) => DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::RGBA(8), DecodingResult::U8(data)) => DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::Gray(8), DecodingResult::U8(data)) => DynamicImage::ImageLuma8(
                image::GrayImage::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::GrayA(8), DecodingResult::U8(data)) => DynamicImage::ImageLumaA8(
                image::ImageBuffer::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::RGB(16), DecodingResult::U16(data)) => DynamicImage::ImageRgb16(
                image::ImageBuffer::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::RGBA(16), DecodingResult::U16(data)) => DynamicImage::ImageRgba16(
                image::ImageBuffer::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::Gray(16), DecodingResult::U16(data)) => DynamicImage::ImageLuma16(
                image::ImageBuffer::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::GrayA(16), DecodingResult::U16(data)) => DynamicImage::ImageLumaA16(
                image::ImageBuffer::from_raw(width, height, data)
                    .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
            ),
            (ColorType::CMYK(8), DecodingResult::U8(data)) => {
                let rgb = data
                    .chunks_exact(4)
                    .flat_map(|p| {
                        p[..3].iter().map(move |c| {
                            ((255 - u16::from(*c)) * (255 - u16::from(p[3])) / 255) as u8
                        })
                    })
                    .collect();
                DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(width, height, rgb)
                        .ok_or_else(|| anyhow::anyhow!("Invalid TIFF image."))?,
                )
            }
            _ => bail!("This TIFF color format is unsupported. Export it as PDF."),
        };
        let mut image = image;
        if let Some(orientation) = image::metadata::Orientation::from_exif(
            decoder
                .get_tag_unsigned::<u16>(tiff::tags::Tag::Orientation)
                .unwrap_or(1) as u8,
        ) {
            image.apply_orientation(orientation);
        }
        document.image(image)?;
        if !decoder.more_images() {
            break;
        }
        decoder.next_image()?;
    }
    document.finish(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_output_is_a_new_pdf_without_source_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("print.pdf");
        let mut document = Document::new("Letter");
        document.image(DynamicImage::new_rgba8(40, 20)).unwrap();
        assert_eq!(document.finish(&output).unwrap(), 1);
        let bytes = fs::read(output).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF"));
        assert!(!text.contains("/JavaScript"));
        assert!(!text.contains("/EmbeddedFiles"));
        assert!(!text.contains("/Annots"));
    }
    #[tokio::test]
    async fn rejects_unsupported_and_invalid_options_before_processing() {
        assert!(
            convert(Path::new("file.exe"), Path::new("print.pdf"), "Letter", 50)
                .await
                .is_err()
        );
        assert!(
            convert(Path::new("file.pdf"), Path::new("print.pdf"), "Letter", 0)
                .await
                .is_err()
        );
    }
}
