//! Native raster implementation for the code-first migration.
//!
//! This is deliberately not named or represented as a Pillow-compatible codec.
//! The canonical watermark/thumbnail policy remains in image_adapters. Known
//! remaining parity work is decoder/color conversion, Lanczos coefficients and
//! rounding, Pillow's box-approximation Gaussian blur, and PNG filter/DEFLATE
//! bytes/metadata. No EXIF orientation is applied, matching the source calls.
use super::{
    ContentError,
    image_adapters::{ImageRasterBackend, Raster},
};
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{
    DynamicImage, ExtendedColorType, GrayImage, ImageEncoder, ImageFormat, ImageReader, RgbaImage,
};
use std::io::Cursor;

/// Explicit native safety budget. Pillow's default decompression-bomb error
/// threshold is 2 * 89_478_485 pixels; image's decoder budget additionally defaults
/// to 512 MiB. That allocation limit is a documented native operational difference.
#[derive(Clone, Debug)]
pub struct NativeImageRasterBackend {
    pub max_pixels: u64,
    pub max_decode_bytes: u64,
}
impl Default for NativeImageRasterBackend {
    fn default() -> Self {
        Self {
            max_pixels: 178_956_970,
            max_decode_bytes: 512 * 1024 * 1024,
        }
    }
}
impl NativeImageRasterBackend {
    pub fn new(max_pixels: u64, max_decode_bytes: u64) -> Result<Self, ContentError> {
        if max_pixels == 0 || max_decode_bytes == 0 {
            return Err(ContentError::new(
                500,
                "Native image limits must be positive",
            ));
        }
        Ok(Self {
            max_pixels,
            max_decode_bytes,
        })
    }
    fn dimensions(&self, width: u32, height: u32) -> Result<usize, ContentError> {
        let pixels = u64::from(width) * u64::from(height);
        if width == 0 || height == 0 || pixels > self.max_pixels {
            return Err(invalid("Image raster dimensions exceed native limits"));
        }
        usize::try_from(pixels).map_err(|_| invalid("Image raster dimensions exceed native limits"))
    }
    fn limits(&self) -> image::Limits {
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(self.max_decode_bytes);
        limits.max_image_width = Some(self.max_pixels.min(u64::from(u32::MAX)) as u32);
        limits.max_image_height = limits.max_image_width;
        limits
    }
    fn buffer(&self, image: &Raster, premultiply: bool) -> Result<RgbaImage, ContentError> {
        image.validate()?;
        let pixels = self.dimensions(image.width, image.height)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(
                pixels
                    .checked_mul(4)
                    .ok_or_else(|| invalid("Image raster too large"))?,
            )
            .map_err(|_| invalid("Image raster allocation failed"))?;
        for pixel in &image.rgba {
            let mut pixel = *pixel;
            if premultiply {
                let alpha = u32::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = div255(u32::from(*channel) * alpha);
                }
            }
            bytes.extend_from_slice(&pixel);
        }
        RgbaImage::from_raw(image.width, image.height, bytes)
            .ok_or_else(|| invalid("Invalid image raster"))
    }
}
fn invalid(message: &str) -> ContentError {
    ContentError::new(422, message)
}
fn div255(value: u32) -> u8 {
    let value = value + 128;
    (((value >> 8) + value) >> 8) as u8
}
impl ImageRasterBackend for NativeImageRasterBackend {
    fn decode(&self, bytes: &[u8], thumbnail: bool) -> Result<Raster, ContentError> {
        if bytes.is_empty() {
            return Err(invalid("Cannot decode an empty image"));
        }
        let format =
            image::guess_format(bytes).map_err(|_| invalid("Image format is not recognized"))?;
        if !matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
        ) {
            return Err(invalid(
                "Image format is not supported by the native backend",
            ));
        }
        // Inspect dimensions before allocating the decoded raster, including when
        // a decoder supports only some of the image crate's allocation limits.
        let mut header = ImageReader::with_format(Cursor::new(bytes), format);
        header.limits(self.limits());
        let (width, height) = header
            .into_dimensions()
            .map_err(|_| invalid("Image header cannot be decoded"))?;
        self.dimensions(width, height)?;
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(self.limits());
        let decoded = reader
            .decode()
            .map_err(|_| invalid("Image cannot be decoded within native limits"))?;
        // Pillow thumbnails retain only RGB/RGBA modes. GIF's first frame and
        // paletted/gray-alpha PNGs are converted to RGB even if native expansion
        // exposed alpha. Watermark inputs always become RGBA.
        let has_alpha = if !thumbnail {
            true
        } else {
            match format {
                ImageFormat::Gif | ImageFormat::Jpeg => false,
                ImageFormat::Png => bytes.get(25) == Some(&6),
                _ => matches!(
                    &decoded,
                    DynamicImage::ImageRgba8(_)
                        | DynamicImage::ImageRgba16(_)
                        | DynamicImage::ImageRgba32F(_)
                ),
            }
        };
        let decoded = decoded.into_rgba8();
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(self.dimensions(decoded.width(), decoded.height())?)
            .map_err(|_| invalid("Image raster allocation failed"))?;
        for pixel in decoded.pixels() {
            let mut pixel = pixel.0;
            if !has_alpha {
                pixel[3] = 255;
            }
            rgba.push(pixel);
        }
        Ok(Raster {
            width: decoded.width(),
            height: decoded.height(),
            rgba,
            has_alpha,
        })
    }
    fn resize_lanczos(
        &self,
        image: &Raster,
        width: u32,
        height: u32,
    ) -> Result<Raster, ContentError> {
        self.dimensions(width, height)?;
        image.validate()?;
        if (width, height) == (image.width, image.height) {
            return Ok(image.clone());
        }
        // imageops requires premultiplied input; explicitly convert so transparent
        // edges do not pull hidden RGB into the visible watermark/thumbnail.
        let alpha = image.has_alpha;
        let source = self.buffer(image, alpha)?;
        let resized = imageops::resize(&source, width, height, FilterType::Lanczos3);
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(self.dimensions(width, height)?)
            .map_err(|_| invalid("Image raster allocation failed"))?;
        for pixel in resized.pixels() {
            let mut pixel = pixel.0;
            if alpha {
                let a = u32::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = if a == 0 {
                        0
                    } else {
                        (u32::from(*channel) * 255 / a).min(255) as u8
                    };
                }
            } else {
                pixel[3] = 255;
            }
            rgba.push(pixel);
        }
        Ok(Raster {
            width,
            height,
            rgba,
            has_alpha: alpha,
        })
    }
    fn gaussian_blur_alpha(
        &self,
        alpha: &[u8],
        width: u32,
        height: u32,
        radius: f64,
    ) -> Result<Vec<u8>, ContentError> {
        if self.dimensions(width, height)? != alpha.len()
            || !radius.is_finite()
            || radius < 0.0
            || radius > f32::MAX as f64
        {
            return Err(invalid("Invalid image alpha filter input"));
        }
        if radius == 0.0 {
            return Ok(alpha.to_vec());
        }
        let gray = GrayImage::from_raw(width, height, alpha.to_vec())
            .ok_or_else(|| invalid("Invalid image alpha raster"))?;
        // image's true Gaussian kernel is usable but not Pillow's GaussianBlur
        // extended-box approximation. Preserve that distinction for consistency.
        Ok(imageops::blur(&gray, radius as f32).into_raw())
    }
    fn encode_png(
        &self,
        image: &Raster,
        keep_alpha: bool,
        compression: u8,
    ) -> Result<Vec<u8>, ContentError> {
        image.validate()?;
        let pixels = self.dimensions(image.width, image.height)?;
        if compression > 9 {
            return Err(invalid("PNG compression level is invalid"));
        }
        let channels = if keep_alpha { 4 } else { 3 };
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(
                pixels
                    .checked_mul(channels)
                    .ok_or_else(|| invalid("Image raster too large"))?,
            )
            .map_err(|_| invalid("Image raster allocation failed"))?;
        for pixel in &image.rgba {
            bytes.extend_from_slice(&pixel[..channels]);
        }
        let mut output = Vec::new();
        PngEncoder::new_with_quality(
            &mut output,
            CompressionType::Level(compression),
            PngFilter::Adaptive,
        )
        .write_image(
            &bytes,
            image.width,
            image.height,
            if keep_alpha {
                ExtendedColorType::Rgba8
            } else {
                ExtendedColorType::Rgb8
            },
        )
        .map_err(|_| invalid("Native PNG encoding failed"))?;
        Ok(output)
    }
}
