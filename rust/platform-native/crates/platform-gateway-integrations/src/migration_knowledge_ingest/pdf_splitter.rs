//! Canonical PDFSplitter range sizing and binary search. Native PDF serialization
//! stays an explicitly supplied PyMuPDF-equivalent backend, never pdftotext.
use super::{IngestError, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Debug)]
pub struct SplitResult {
    pub part_index: usize,
    pub page_start: usize,
    pub page_end: usize,
    pub path: PathBuf,
    pub size_bytes: u64,
}
pub trait PdfSplitBackend: Send + Sync {
    fn page_count(&self, source: &Path) -> Result<usize>;
    fn write_pages(
        &self,
        source: &Path,
        start: usize,
        end: usize,
        destination: &Path,
    ) -> Result<()>;
}
#[derive(Clone)]
pub struct PdfSplitter {
    pub max_size_bytes: u64,
    pub min_pages_per_part: usize,
    pub backend: Arc<dyn PdfSplitBackend>,
}
impl PdfSplitter {
    pub fn new(backend: Arc<dyn PdfSplitBackend>) -> Self {
        Self {
            backend,
            max_size_bytes: 20 * 1024 * 1024,
            min_pages_per_part: 5,
        }
    }
    fn write_part(
        &self,
        source: &Path,
        start: usize,
        end: usize,
        index: usize,
        tmp: &Path,
    ) -> Result<PathBuf> {
        let path = tmp.join(format!(
            "pdfsplit_{}_part{index:03}.pdf",
            uuid::Uuid::new_v4()
        ));
        self.backend.write_pages(source, start, end, &path)?;
        Ok(path)
    }
    pub fn split_pdf(&self, source: &Path, tmp: &Path) -> Result<Vec<SplitResult>> {
        let size = fs::metadata(source)?.len();
        if size <= self.max_size_bytes {
            return Ok(vec![SplitResult {
                part_index: 0,
                page_start: 0,
                page_end: 0,
                path: source.to_path_buf(),
                size_bytes: size,
            }]);
        }
        let total = self.backend.page_count(source)?;
        if total == 0 {
            return Ok(vec![]);
        }
        if self.max_size_bytes == 0 {
            return Err(IngestError::Validation(
                "PDF split size must be positive".into(),
            ));
        }
        let minimum = self.min_pages_per_part.max(1);
        let estimate = minimum
            .max((self.max_size_bytes as f64 * 0.85 / (size as f64 / total as f64)) as usize);
        let mut out = vec![];
        let mut cursor = 0;
        while cursor < total {
            let mut end = (cursor + estimate).min(total);
            if end - cursor < minimum && end < total {
                end = (cursor + minimum).min(total)
            }
            let mut path = self.write_part(source, cursor, end, out.len(), tmp)?;
            let mut bytes = fs::metadata(&path)?.len();
            if bytes > self.max_size_bytes && end - cursor > minimum {
                fs::remove_file(&path)?;
                let (mut lo, mut hi) = (cursor + minimum, end);
                let mut best = lo;
                while lo <= hi {
                    let mid = (lo + hi) / 2;
                    let probe = self.write_part(source, cursor, mid, out.len(), tmp)?;
                    let probe_size = fs::metadata(&probe)?.len();
                    fs::remove_file(probe)?;
                    if probe_size <= self.max_size_bytes {
                        best = mid;
                        lo = mid + 1
                    } else {
                        hi = mid.saturating_sub(1)
                    }
                }
                end = best;
                path = self.write_part(source, cursor, end, out.len(), tmp)?;
                bytes = fs::metadata(&path)?.len();
            }
            out.push(SplitResult {
                part_index: out.len(),
                page_start: cursor,
                page_end: end,
                path,
                size_bytes: bytes,
            });
            cursor = end;
        }
        Ok(out)
    }
}
