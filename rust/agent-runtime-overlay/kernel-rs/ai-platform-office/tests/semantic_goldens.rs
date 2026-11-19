use ai_platform_office::{
    Block, DesignSystem, DocumentFormat, DocumentGenerator, GenerateDocumentRequest, Inline,
};
use std::io::Read;

fn archive_entry(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid archive");
    let mut entry = archive.by_name(name).expect("archive entry");
    let mut output = String::new();
    entry.read_to_string(&mut output).expect("UTF-8 XML");
    output
}

fn request(
    format: DocumentFormat,
    title: &str,
    locale: &str,
    design_system: DesignSystem,
    body: &str,
) -> GenerateDocumentRequest {
    GenerateDocumentRequest {
        format,
        title: title.to_owned(),
        goal: "Golden semantic fixture".to_owned(),
        body_markdown: Some(body.to_owned()),
        locale: locale.to_owned(),
        design_system: Some(design_system),
        template_name: None,
    }
}

#[test]
fn english_and_chinese_fixtures_cover_semantic_fields() {
    let en: serde_json::Value = serde_json::from_str(include_str!("../fixtures/en_semantic.json"))
        .expect("English fixture");
    let zh: serde_json::Value = serde_json::from_str(include_str!("../fixtures/zh_semantic.json"))
        .expect("Chinese fixture");
    assert_eq!(en["locale"], "en-US");
    assert_eq!(zh["locale"], "zh-CN");
    assert_eq!(en["design_system"], "enterprise");
    assert_eq!(zh["design_system"], "editorial");
}

#[test]
fn visual_input_fixture_describes_semantic_unpack_contract() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/visual_input.json"))
            .expect("visual input fixture");
    assert_eq!(fixture["format"], "pptx");
    assert!(fixture["required_parts"].as_array().unwrap().len() >= 4);
    assert_eq!(fixture["visual_acceptance"], "requires_office_viewer");
}

#[test]
fn ooxml_formats_have_the_same_semantic_ir() {
    let body = "# Summary\n\n- one\n- two\n\n| Metric | Value |\n| --- | --- |\n| Total | =SUM(1,2) |\n\n> conclusion\n\n```rust\nlet x = 1;\n```";
    let generator = DocumentGenerator::new();
    let mut semantic = None;
    for format in [
        DocumentFormat::Docx,
        DocumentFormat::Pptx,
        DocumentFormat::Xlsx,
    ] {
        let result = generator
            .generate(&request(
                format,
                "Golden",
                "en-US",
                DesignSystem::Enterprise,
                body,
            ))
            .expect("artifact");
        if let Some(previous) = &semantic {
            assert_eq!(previous, &result.semantic_ir);
        } else {
            semantic = Some(result.semantic_ir.clone());
        }
        assert!(result.size_bytes > 0);
        assert_eq!(result.size_bytes, result.bytes.len());
    }
    let ir = semantic.expect("semantic IR");
    assert!(
        ir.blocks
            .iter()
            .any(|block| matches!(block, Block::Table { .. }))
    );
    assert!(
        ir.blocks
            .iter()
            .any(|block| matches!(block, Block::Code { .. }))
    );
}

#[test]
fn pptx_roundtrips_through_the_public_reader() {
    let generator = DocumentGenerator::new();
    let result = generator
        .generate(&request(
            DocumentFormat::Pptx,
            "中文标题",
            "zh-CN",
            DesignSystem::Editorial,
            "# 第一章\n\n正文 **粗体** 与 *斜体*。\n\n- 条目一\n- 条目二\n\n| 指标 | 数值 |\n| --- | --- |\n| 延迟 | 3 秒 |\n\n# 第二章\n\n继续说明。",
        ))
        .expect("pptx");
    let presentation =
        powerpoint_ooxml::Presentation::read_from(std::io::Cursor::new(result.bytes.as_slice()))
            .expect("generated package should be readable");
    assert_eq!(presentation.slides.len(), 3);
    assert!(
        presentation.slides[1]
            .shapes
            .iter()
            .any(|shape| matches!(shape, powerpoint_ooxml::Shape::Table(_)))
    );

    let slide = archive_entry(&result.bytes, "ppt/slides/slide2.xml");
    assert!(slide.contains("第一章"));
    assert!(slide.matches("<p:sp>").count() >= 3);
    assert!(slide.contains(" b=\"1\""));
    assert!(slide.contains(" i=\"1\""));
    assert!(slide.contains("<a:buChar"));
    assert!(slide.contains("<a:tbl>"));
    assert!(slide.contains("<a:tr"));
    assert!(slide.contains("Noto Sans CJK SC"));
    assert!(archive_entry(&result.bytes, "ppt/slides/slide3.xml").contains("第二章"));
    for required in [
        "ppt/slideMasters/slideMaster1.xml",
        "ppt/slideLayouts/slideLayout1.xml",
        "ppt/theme/theme1.xml",
        "ppt/_rels/presentation.xml.rels",
        "ppt/slides/_rels/slide1.xml.rels",
    ] {
        let _ = archive_entry(&result.bytes, required);
    }
}

#[test]
fn pdf_without_a_controlled_font_fails_closed_for_chinese_input() {
    let generator = DocumentGenerator::new();
    let error = generator
        .generate(&request(
            DocumentFormat::Pdf,
            "中文标题",
            "zh-CN",
            DesignSystem::Editorial,
            "中文正文",
        ))
        .expect_err("font bytes are mandatory");
    assert!(error.to_string().contains("controlled font bytes"));
}

#[test]
fn docx_and_xlsx_are_real_ooxml_archives() {
    let generator = DocumentGenerator::new();
    for format in [DocumentFormat::Docx, DocumentFormat::Xlsx] {
        let result = generator
            .generate(&request(
                format,
                "Archive fixture",
                "en-US",
                DesignSystem::Enterprise,
                "body",
            ))
            .expect("ooxml");
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(result.bytes)).expect("valid zip archive");
        assert!(archive.by_name("[Content_Types].xml").is_ok());
        let required = match format {
            DocumentFormat::Docx => "word/document.xml",
            DocumentFormat::Xlsx => "xl/workbook.xml",
            _ => unreachable!(),
        };
        assert!(archive.by_name(required).is_ok());
    }
}

#[test]
fn image_and_formula_inlines_are_semantic_and_images_are_embedded() {
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let request = request(
        DocumentFormat::Docx,
        "Image and formula",
        "en-US",
        DesignSystem::Enterprise,
        &format!("![pixel](data:image/png;base64,{png}) and $x^2$"),
    );
    let result = DocumentGenerator::new().generate(&request).expect("docx");
    assert!(result
        .semantic_ir
        .blocks
        .iter()
        .any(|block| matches!(block, Block::Paragraph { inlines } if inlines.iter().any(|inline| matches!(inline, Inline::Image { .. })) && inlines.iter().any(|inline| matches!(inline, Inline::Formula { .. })))));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(result.bytes)).expect("zip");
    assert!(archive.by_name("word/media/image1.png").is_ok());
}

#[test]
fn preview_and_existing_edit_preserve_unknown_parts_by_content() {
    let generator = DocumentGenerator::new();
    let original = generator
        .generate(&request(
            DocumentFormat::Docx,
            "Original",
            "en-US",
            DesignSystem::Enterprise,
            "before",
        ))
        .expect("docx");
    let source;
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(original.bytes)).expect("zip");
        let mut output = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for index in 0..archive.len() {
            let mut part = archive.by_index(index).expect("part");
            let name = part.name().to_owned();
            let mut content = Vec::new();
            part.read_to_end(&mut content).expect("content");
            output.start_file(name, options).expect("file");
            std::io::Write::write_all(&mut output, &content).expect("write");
        }
        output
            .start_file("custom/unknown.bin", options)
            .expect("unknown");
        std::io::Write::write_all(&mut output, b"preserve-me").expect("unknown content");
        source = output.finish().expect("finish").into_inner();
    }
    let preview = generator
        .preview(DocumentFormat::Docx, &source)
        .expect("preview");
    assert!(
        preview
            .unknown_parts
            .iter()
            .any(|name| name == "custom/unknown.bin")
    );
    let edited = generator
        .modify_existing(
            DocumentFormat::Docx,
            &source,
            &request(
                DocumentFormat::Docx,
                "Edited",
                "en-US",
                DesignSystem::Enterprise,
                "after",
            ),
        )
        .expect("edit");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(edited.bytes)).expect("edited zip");
    let mut unknown = archive.by_name("custom/unknown.bin").expect("unknown part");
    let mut content = Vec::new();
    unknown.read_to_end(&mut content).expect("unknown content");
    assert_eq!(content, b"preserve-me");
}

#[test]
fn preview_rejects_zip_slip_parts_before_unpacking() {
    let mut output = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    output
        .start_file("../escape.xml", zip::write::SimpleFileOptions::default())
        .expect("file");
    std::io::Write::write_all(&mut output, b"unsafe").expect("content");
    let bytes = output.finish().expect("finish").into_inner();
    assert!(
        DocumentGenerator::new()
            .preview(DocumentFormat::Docx, &bytes)
            .is_err()
    );
}

#[test]
fn docx_preserves_heading_inline_list_table_quote_and_code_structure() {
    let generator = DocumentGenerator::new();
    let result = generator
        .generate(&request(
            DocumentFormat::Docx,
            "中文结构测试",
            "zh-CN",
            DesignSystem::Enterprise,
            "# 一级标题\n\n正文 **粗体**、*斜体* 与 `代码`。\n\n- 条目一\n- 条目二\n\n| 列一 | 列二 |\n| --- | --- |\n| 值一 | 值二 |\n\n> 引用内容\n\n```rust\nlet answer = 42;\n```",
        ))
        .expect("docx");
    let document = archive_entry(&result.bytes, "word/document.xml");
    let numbering = archive_entry(&result.bytes, "word/numbering.xml");
    assert!(document.contains("w:pStyle w:val=\"Heading1\""));
    assert!(document.contains("<w:b"));
    assert!(document.contains("<w:i"));
    assert!(document.contains("w:ascii=\"Consolas\""));
    assert!(document.contains("w:eastAsia=\"Noto Sans CJK SC\""));
    assert!(document.contains("<w:numPr>"));
    assert!(document.contains("<w:tbl>"));
    assert!(document.contains("<w:tr>"));
    assert!(document.contains("<w:tc>"));
    assert!(document.contains("w:pStyle w:val=\"Quote\""));
    assert!(numbering.contains("w:numFmt w:val=\"bullet\""));
    assert!(numbering.contains("w:lvlText w:val=\"•\""));
}
