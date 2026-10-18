//! Concrete fetch and image policy adapters. Raster codecs are explicit because
//! Pillow's decoding, Lanczos and Gaussian rounding are part of the source contract.
use super::images::{FetchedImage, ImageReferenceFetch, ImageTransforms};
use super::{ContentError, ContentFuture};
use crate::connector_oauth_http::{
    ConnectorDnsResolver, NativeConnectorDnsResolver, oauth_form_safe_address,
};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

pub struct PinnedImageFetch {
    pub resolver: Arc<dyn ConnectorDnsResolver>,
    pub allowed_hosts: Option<HashSet<String>>,
    pub max_redirects: usize,
}
impl Default for PinnedImageFetch {
    fn default() -> Self {
        Self {
            resolver: Arc::new(NativeConnectorDnsResolver),
            allowed_hosts: None,
            max_redirects: 3,
        }
    }
}
impl ImageReferenceFetch for PinnedImageFetch {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
        timeout: Duration,
    ) -> ContentFuture<'a, FetchedImage> {
        Box::pin(async move {
            let invalid = || ContentError::new(422, "invalid_image_url");
            if max_bytes == 0 {
                return Err(invalid());
            }
            let mut current = reqwest::Url::parse(url).map_err(|_| invalid())?;
            let mut seen = HashSet::new();
            for hop in 0..=self.max_redirects {
                if !matches!(current.scheme(), "http" | "https")
                    || !seen.insert(current.as_str().to_owned())
                {
                    return Err(invalid());
                }
                let host = current
                    .host_str()
                    .ok_or_else(invalid)?
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if self.allowed_hosts.as_ref().is_some_and(|hosts| {
                    !hosts.iter().any(|suffix| {
                        let suffix = suffix.to_lowercase();
                        let suffix = suffix.trim_start_matches('.');
                        host == suffix || host.ends_with(&format!(".{suffix}"))
                    })
                }) {
                    return Err(invalid());
                }
                let port = current.port_or_known_default().ok_or_else(invalid)?;
                let addresses = if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                    vec![ip]
                } else {
                    self.resolver
                        .resolve(&host, port)
                        .await
                        .map_err(|_| invalid())?
                };
                if addresses.is_empty() || addresses.iter().any(|a| !oauth_form_safe_address(*a)) {
                    return Err(invalid());
                }
                let address = addresses[0];
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .connect_timeout(timeout)
                    .read_timeout(timeout)
                    .no_proxy()
                    .resolve(&host, SocketAddr::new(address, port))
                    .build()
                    .map_err(|_| invalid())?;
                // Keep the logical URL host for TLS SNI and use source's original Host header.
                let mut response = client
                    .get(current.clone())
                    .header(reqwest::header::HOST, &host)
                    .send()
                    .await
                    .map_err(|_| invalid())?;
                if response.status().is_redirection() {
                    if hop == self.max_redirects {
                        return Err(invalid());
                    }
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(invalid)?;
                    current = current.join(location).map_err(|_| invalid())?;
                    continue;
                }
                if !response.status().is_success() {
                    return Err(invalid());
                }
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
                    if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                        return Err(ContentError::new(413, "image exceeds maximum size"));
                    }
                    bytes.extend_from_slice(&chunk)
                }
                return Ok(FetchedImage {
                    bytes,
                    content_type,
                    status,
                });
            }
            Err(invalid())
        })
    }
}

#[derive(Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[u8; 4]>,
    pub has_alpha: bool,
}
impl Raster {
    pub fn validate(&self) -> Result<(), ContentError> {
        if self.width == 0
            || self.height == 0
            || u64::from(self.width) * u64::from(self.height) != self.rgba.len() as u64
        {
            Err(ContentError::new(422, "Invalid image raster"))
        } else {
            Ok(())
        }
    }
}
/// Raster codec boundary. The source contract uses Pillow. The explicitly
/// selected NativeImageRasterBackend supplies a usable Rust implementation with
/// documented pixel/filter/encoder differences; it is not claimed Pillow-exact.
pub trait ImageRasterBackend: Send + Sync {
    fn decode(&self, bytes: &[u8], thumbnail: bool) -> Result<Raster, ContentError>;
    fn resize_lanczos(
        &self,
        image: &Raster,
        width: u32,
        height: u32,
    ) -> Result<Raster, ContentError>;
    fn gaussian_blur_alpha(
        &self,
        alpha: &[u8],
        width: u32,
        height: u32,
        radius: f64,
    ) -> Result<Vec<u8>, ContentError>;
    fn encode_png(
        &self,
        image: &Raster,
        keep_alpha: bool,
        compression: u8,
    ) -> Result<Vec<u8>, ContentError>;
}
pub struct AdaptiveImageTransforms {
    pub backend: Arc<dyn ImageRasterBackend>,
    pub watermark: Arc<Raster>,
}
impl AdaptiveImageTransforms {
    pub fn new(
        backend: Arc<dyn ImageRasterBackend>,
        watermark_png: &[u8],
    ) -> Result<Self, ContentError> {
        let watermark = backend.decode(watermark_png, false)?;
        watermark.validate()?;
        Ok(Self {
            backend,
            watermark: Arc::new(watermark),
        })
    }
    pub fn watermark_bytes(&self, raw: &[u8]) -> Result<Vec<u8>, ContentError> {
        let base = self.backend.decode(raw, false)?;
        base.validate()?;
        let target = (base.width as f64 * 0.22) as u32;
        let scale = target as f64 / self.watermark.width as f64;
        let width = ((self.watermark.width as f64 * scale) as u32).max(1);
        let height = ((self.watermark.height as f64 * scale) as u32).max(1);
        let mark = self
            .backend
            .resize_lanczos(&self.watermark, width, height)?;
        mark.validate()?;
        let (x, y, brightness) = pick_corner(&base, width, height);
        let (fill, outline) = if brightness > 140.0 {
            ([20, 20, 20], [255, 255, 255])
        } else {
            ([255, 255, 255], [0, 0, 0])
        };
        let alpha = mark.rgba.iter().map(|p| p[3]).collect::<Vec<_>>();
        let mut dilated = vec![0; alpha.len()];
        for py in 0..height {
            for px in 0..width {
                let mut maximum = 0;
                for dy in -2i64..=2 {
                    for dx in -2i64..=2 {
                        let xx = (px as i64 + dx).clamp(0, width as i64 - 1) as usize;
                        let yy = (py as i64 + dy).clamp(0, height as i64 - 1) as usize;
                        maximum = maximum.max(alpha[yy * width as usize + xx]);
                    }
                }
                dilated[py as usize * width as usize + px as usize] = maximum;
            }
        }
        let blurred = self
            .backend
            .gaussian_blur_alpha(&dilated, width, height, 0.8)?;
        if blurred.len() != alpha.len() {
            return Err(ContentError::new(500, "Invalid image filter output"));
        }
        let mut overlay = vec![[0u8; 4]; base.rgba.len()];
        for py in 0..height as i64 {
            for px in 0..width as i64 {
                let bx = x + px;
                let by = y + py;
                if bx < 0 || by < 0 || bx >= base.width as i64 || by >= base.height as i64 {
                    continue;
                }
                let i = (py * width as i64 + px) as usize;
                let dest = &mut overlay[(by * base.width as i64 + bx) as usize];
                let ring = (i16::from(blurred[i]) - i16::from(alpha[i] / 2)).clamp(0, 255) as u8;
                let oa = (f64::from(ring) * 0.85) as u8;
                let fa = (f64::from(alpha[i]) * 0.75) as u8;
                paste(dest, [outline[0], outline[1], outline[2], oa], oa);
                paste(dest, [fill[0], fill[1], fill[2], fa], fa);
            }
        }
        let mut result = base;
        for (pixel, over) in result.rgba.iter_mut().zip(overlay) {
            *pixel = alpha_composite(*pixel, over);
        }
        result.has_alpha = false;
        self.backend.encode_png(&result, false, 1)
    }
    pub fn thumbnail_bytes(&self, raw: &[u8], max_edge: u32) -> Result<Vec<u8>, ContentError> {
        let mut image = self.backend.decode(raw, true)?;
        image.validate()?;
        let scale = (max_edge as f64 / image.width.max(image.height) as f64).min(1.0);
        if scale < 1.0 {
            let width = ((image.width as f64 * scale) as u32).max(1);
            let height = ((image.height as f64 * scale) as u32).max(1);
            image = self.backend.resize_lanczos(&image, width, height)?;
        }
        self.backend.encode_png(&image, image.has_alpha, 6)
    }
}
impl ImageTransforms for AdaptiveImageTransforms {
    fn watermark<'a>(&'a self, raw: Vec<u8>) -> ContentFuture<'a, (Vec<u8>, String)> {
        let backend = self.backend.clone();
        let watermark = self.watermark.clone();
        Box::pin(async move {
            let effect = ai_platform_gateway_foundation::worker_lease::current_effect();
            let out = tokio::task::spawn_blocking(move || {
                let _effect = effect;
                Self { backend, watermark }.watermark_bytes(&raw)
            })
            .await
            .map_err(|_| ContentError::new(500, "Image watermarking failed"))??;
            Ok((out, "image/png".into()))
        })
    }
    fn thumbnail<'a>(&'a self, raw: Vec<u8>) -> ContentFuture<'a, Option<Vec<u8>>> {
        let backend = self.backend.clone();
        let watermark = self.watermark.clone();
        Box::pin(async move {
            let effect = ai_platform_gateway_foundation::worker_lease::current_effect();
            Ok(tokio::task::spawn_blocking(move || {
                let _effect = effect;
                Self { backend, watermark }.thumbnail_bytes(&raw, 256)
            })
            .await
            .ok()
            .and_then(Result::ok))
        })
    }
}
fn div255(v: u32) -> u8 {
    let v = v + 128;
    (((v >> 8) + v) >> 8) as u8
}
fn paste(dst: &mut [u8; 4], src: [u8; 4], mask: u8) {
    for i in 0..4 {
        dst[i] = div255(
            u32::from(dst[i]) * (255 - u32::from(mask)) + u32::from(src[i]) * u32::from(mask),
        );
    }
}
// Pillow alpha_composite's 7-bit fixed-point coefficient rounding.
fn alpha_composite(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    if src[3] == 0 {
        return dst;
    }
    let blend = u32::from(dst[3]) * (255 - u32::from(src[3]));
    let outa = u32::from(src[3]) * 255 + blend;
    let c1 = u32::from(src[3]) * 255 * 255 * 128 / outa;
    let c2 = 255 * 128 - c1;
    let mut out = [0; 4];
    for i in 0..3 {
        let tmp = u32::from(src[i]) * c1 + u32::from(dst[i]) * c2 + 0x80 * 128;
        out[i] = (((tmp >> 8) + tmp) >> 8 >> 7) as u8;
    }
    out[3] = div255(outa);
    out
}
fn pick_corner(base: &Raster, ww: u32, wh: u32) -> (i64, i64, f32) {
    let (w, h, ww, wh) = (base.width as i64, base.height as i64, ww as i64, wh as i64);
    let positions = [
        (24, h - wh - 24),
        (w - ww - 24, h - wh - 24),
        (24, 24),
        (w - ww - 24, 24),
    ];
    let mut best = None;
    for (x, y) in positions {
        let (x0, y0, x1, y1) = (
            (x - 6).max(0),
            (y - 6).max(0),
            (x + ww + 6).min(w),
            (y + wh + 6).min(h),
        );
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let mut gray = Vec::new();
        for yy in y0..y1 {
            for xx in x0..x1 {
                let p = base.rgba[(yy * w + xx) as usize];
                gray.push(
                    ((u32::from(p[0]) * 19595
                        + u32::from(p[1]) * 38470
                        + u32::from(p[2]) * 7471
                        + 32768)
                        >> 16) as f32,
                );
            }
        }
        let mean = pairwise_sum(&gray) / gray.len() as f32;
        for v in &mut gray {
            *v = (*v - mean) * (*v - mean)
        }
        let std = (pairwise_sum(&gray) / gray.len() as f32).sqrt();
        if best.is_none_or(|(_, _, _, s)| std < s) {
            best = Some((x, y, mean, std))
        }
    }
    best.map(|(x, y, m, _)| (x, y, m))
        .unwrap_or((w - ww - 24, h - wh - 24, 128.0))
}
fn pairwise_sum(v: &[f32]) -> f32 {
    if v.len() < 8 {
        return v.iter().fold(-0.0, |a, b| a + b);
    }
    if v.len() <= 128 {
        let mut r = [0f32; 8];
        r.copy_from_slice(&v[..8]);
        let mut i = 8;
        while i + 7 < v.len() {
            for j in 0..8 {
                r[j] += v[i + j]
            }
            i += 8
        }
        let mut sum = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for value in &v[i..] {
            sum += value
        }
        sum
    } else {
        let n = (v.len() / 2) / 8 * 8;
        pairwise_sum(&v[..n]) + pairwise_sum(&v[n..])
    }
}
