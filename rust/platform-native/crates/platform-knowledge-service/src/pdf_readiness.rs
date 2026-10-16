//! Startup-only proof that the selected PDF backend can extract a bounded page.
//! This probe is not evidence of canonical pypdf/PyMuPDF parity.
use super::{KnowledgeError, Result};
use crate::migration_knowledge_ingest::parser::DocumentParser;

pub(super) fn probe(parser: &DocumentParser) -> Result<()> {
    let bytes = probe_document();
    let (text, _) = parser
        .extract_text_from_bytes(&bytes, "knowledge-readiness.pdf", "application/pdf")
        .map_err(|_| KnowledgeError::Startup("configured_pdf_backend_probe"))?;
    if text.split_whitespace().collect::<Vec<_>>().join(" ") != "Native PDF readiness" {
        return Err(KnowledgeError::Startup("configured_pdf_backend_probe"));
    }
    Ok(())
}

fn probe_document() -> Vec<u8> {
    let stream = "BT /F1 12 Tf 30 80 Td (Native PDF readiness) Tj ET\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
    ];
    let mut document = String::from("%PDF-1.4\n");
    let mut offsets = vec![];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(document.len());
        document.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = document.len();
    document.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        document.push_str(&format!("{offset:010} 00000 n \n"));
    }
    document.push_str(&format!(
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    document.into_bytes()
}
