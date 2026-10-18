//! Real Poppler PDF backend tests; tools are discovered only for this module.
use super::*;

#[cfg(target_os = "macos")]
#[test]
fn macos_process_limits_start_and_sample_child_footprint() {
    let mut command = Command::new("/bin/sleep");
    command.arg("5");
    configure_child(&mut command, Duration::from_secs(2), 64 * 1024 * 1024, 4096).unwrap();
    let child = command.spawn().unwrap();
    let mut child = ChildGuard {
        child,
        finished: false,
    };
    // Any successfully started process must exceed this deliberately tiny
    // test ceiling. This exercises the Darwin sampler without large allocs.
    assert!(darwin_memory_limit_exceeded(child.child.id(), 1).unwrap());
    child.terminate();
}

#[cfg(target_os = "macos")]
#[test]
fn macos_fast_child_exit_does_not_fail_memory_sampling() {
    let mut command = Command::new("/usr/bin/true");
    configure_child(&mut command, Duration::from_secs(2), 64 * 1024 * 1024, 4096).unwrap();
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None => match sample_darwin_child_memory(&mut child, u64::MAX).unwrap() {
                DarwinMemorySample::Exited(status) => break status,
                DarwinMemorySample::WithinLimit => (),
                DarwinMemorySample::Exceeded => {
                    panic!("fast child exceeded an unlimited test cap")
                }
            },
        }
        assert!(Instant::now() < deadline, "fast child did not exit in time");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(status.success());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_footprint_error_is_tolerated_only_after_child_exit() {
    let mut exited_command = Command::new("/usr/bin/true");
    configure_child(
        &mut exited_command,
        Duration::from_secs(2),
        64 * 1024 * 1024,
        4096,
    )
    .unwrap();
    let mut exited_child = exited_command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if exited_child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "fast child did not exit in time");
        std::thread::sleep(Duration::from_millis(1));
    }
    match reconcile_darwin_memory_sample(
        &mut exited_child,
        Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
    )
    .unwrap()
    {
        DarwinMemorySample::Exited(status) => assert!(status.success()),
        _ => panic!("exited child was not recognized"),
    }

    let mut live_command = Command::new("/bin/sleep");
    live_command.arg("5");
    configure_child(
        &mut live_command,
        Duration::from_secs(2),
        64 * 1024 * 1024,
        4096,
    )
    .unwrap();
    let live_child = live_command.spawn().unwrap();
    let mut live_child = ChildGuard {
        child: live_child,
        finished: false,
    };
    let error = reconcile_darwin_memory_sample(
        &mut live_child.child,
        Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
    )
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
    live_child.terminate();
}

/// PATH discovery is test-only; production continues to require configured
/// absolute executables and rechecks their trusted permissions before spawn.
fn installed_poppler(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH")
        .expect("real Poppler PDF tests require PATH with installed Poppler tools");
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        let Ok(info) = fs::metadata(&candidate) else {
            continue;
        };
        if !info.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if info.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        let absolute = candidate.canonicalize().unwrap_or_else(|error| {
            panic!("cannot resolve installed Poppler tool {name} to an absolute path: {error}")
        });
        return executable(&absolute).unwrap_or_else(|error| {
            panic!("installed Poppler tool {name} fails production executable validation: {error}")
        });
    }
    panic!(
        "real Poppler PDF tests require installed executable {name} on PATH; install Poppler or fix the test environment"
    );
}

fn backend() -> PopplerPdfBackend {
    let mut config = PopplerPdfConfig::new(
        installed_poppler("pdfinfo"),
        installed_poppler("pdftotext"),
        Some(installed_poppler("pdftoppm")),
        std::env::temp_dir().join("knowledge-native-pdf-tests"),
    );
    config.allow_ingestion_text_alternative = true;
    config.pdfimages = Some(installed_poppler("pdfimages"));
    config.pdfseparate = Some(installed_poppler("pdfseparate"));
    config.pdfunite = Some(installed_poppler("pdfunite"));
    PopplerPdfBackend::new(config).unwrap()
}
fn fixture() -> Vec<u8> {
    let text = b"BT /F1 12 Tf 30 80 Td (Native PDF readiness) Tj ET\n";
    let image_content = b"q 64 0 0 64 20 20 cm /Im1 Do Q\n";
    let mut pixel = vec![];
    for _ in 0..64 * 64 {
        pixel.extend_from_slice(&[210, 40, 60]);
    }
    let stream = |data: &[u8], dictionary: &str| {
        let mut o = format!("<< /Length {} {dictionary} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    let objects=vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Resources << /XObject << /Im1 7 0 R >> >> /Contents 8 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream(text,""),
        stream(&pixel,"/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceRGB /BitsPerComponent 8"),
        stream(image_content,""),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(obj);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
#[test]
fn native_text_and_raster_decode_real_pdf() {
    let backend = backend();
    let pdf = fixture();
    let mut document = backend.open_pymupdf(&pdf).unwrap();
    assert_eq!(document.page_count(), 2);
    assert_eq!(
        document.page_text(0).unwrap().trim(),
        "Native PDF readiness"
    );
    let image = PdfRasterBackend::render(&backend, &pdf, 1, Some(72)).unwrap();
    assert!(image.png.starts_with(b"\x89PNG"));
    assert_eq!((image.width, image.height), (300, 144));
}
#[test]
fn native_embedded_image_keeps_pdf_xref() {
    let backend = backend();
    let images = backend.page_images(&fixture(), 1).unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(
        (images[0].width, images[0].height, images[0].xref),
        (64, 64, 7)
    );
    assert!(images[0].bytes.starts_with(b"\x89PNG"));
}
#[test]
fn native_streaming_extracts_images() {
    let backend = backend();
    let page = StreamingPdfBackend::extract_page(
        &backend,
        &PdfSource::Bytes(Arc::new(fixture())),
        1,
        true,
        false,
        32,
    )
    .unwrap();
    assert_eq!(page.page_number, 2);
    assert_eq!(page.images.len(), 1);
    let decoded = image::load_from_memory(&page.images[0]).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (32, 32));
}
#[test]
fn native_split_serializes_selected_page() {
    let backend = backend();
    let scratch = Scratch::new(&backend.config.scratch_root).unwrap();
    let source = scratch.path.join("original.pdf");
    fs::write(&source, fixture()).unwrap();
    let target = scratch.path.join("selected.pdf");
    PdfSplitBackend::write_pages(&backend, &source, 1, 2, &target).unwrap();
    assert_eq!(PdfSplitBackend::page_count(&backend, &target).unwrap(), 1);
    assert_eq!(
        backend
            .page_images(&fs::read(target).unwrap(), 0)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn native_split_rejects_invalid_ranges_without_destination() {
    let backend = backend();
    let scratch = Scratch::new(&backend.config.scratch_root).unwrap();
    let source = scratch.path.join("original.pdf");
    fs::write(&source, fixture()).unwrap();
    let target = scratch.path.join("selected.pdf");
    assert!(PdfSplitBackend::write_pages(&backend, &source, 2, 1, &target).is_err());
    assert!(!target.exists());
}
