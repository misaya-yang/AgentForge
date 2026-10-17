// AgentForge downstream zip9 compatibility regression; not an upstream Candle file.
// SPDX-License-Identifier: Apache-2.0
use candle_core::{Device, Tensor};
use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
};
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "agentforge-candle-zip9-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn npz_tensor_write_read_by_name_and_lazy_loader_keep_values() -> candle_core::Result<()> {
    let tmp = Temporary::new();
    let path = tmp.path("embeddings.npz");
    let tensor = Tensor::from_vec(vec![1f32, 2., 3., 4., 5., 6.], (2, 3), &Device::Cpu)?;
    Tensor::write_npz(&[("embedding", &tensor)], &path)?;
    let eager = Tensor::read_npz(&path)?;
    assert_eq!(eager.len(), 1);
    assert_eq!(eager[0].0, "embedding");
    assert_eq!(eager[0].1.to_vec2::<f32>()?, tensor.to_vec2::<f32>()?);
    let named = Tensor::read_npz_by_name(&path, &["embedding"])?;
    assert_eq!(named[0].dims(), &[2, 3]);
    let lazy = candle_core::npy::NpzTensors::new(&path)?;
    let (shape, dtype) = lazy.get_shape_and_dtype("embedding")?;
    assert_eq!(shape.dims(), &[2, 3]);
    assert_eq!(dtype, candle_core::DType::F32);
    assert_eq!(
        lazy.get("embedding")?.unwrap().to_vec2::<f32>()?,
        tensor.to_vec2::<f32>()?
    );
    assert!(lazy.get("missing")?.is_none());
    Ok(())
}
#[test]
fn zip9_owned_utf8_names_survive_npz_index() -> candle_core::Result<()> {
    let tmp = Temporary::new();
    let path = tmp.path("unicode.npz");
    let tensor = Tensor::new(&[0.25f32, 0.75], &Device::Cpu)?;
    Tensor::write_npz(&[("嵌入向量", &tensor)], &path)?;
    let values = Tensor::read_npz(&path)?;
    assert_eq!(values[0].0, "嵌入向量");
    let lazy = candle_core::npy::NpzTensors::new(&path)?;
    assert_eq!(
        lazy.get("嵌入向量")?.unwrap().to_vec1::<f32>()?,
        vec![0.25, 0.75]
    );
    Ok(())
}
fn raw_non_utf8_name() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "data.pkl",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"fixture").unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let local = bytes.windows(4).position(|w| w == b"PK\x03\x04").unwrap();
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    for (start, flags, name) in [(local, 6, 30), (central, 8, 46)] {
        let value =
            u16::from_le_bytes([bytes[start + flags], bytes[start + flags + 1]]) | (1 << 11);
        bytes[start + flags..start + flags + 2].copy_from_slice(&value.to_le_bytes());
        bytes[start + name] = 0xff;
    }
    bytes
}
#[test]
fn malformed_zip9_filename_is_a_zip_error_for_both_npz_and_pickle() {
    let tmp = Temporary::new();
    let path = tmp.path("invalid-name.zip");
    // zip9 intentionally decodes arbitrary non-UTF8 bytes as CP437, even with
    // the language flag set. That is not malformed metadata. Instead declare a
    // central-directory filename that cannot fit in this archive: read_exact
    // must fail before either NPY parsing or pickle interpretation is reached.
    let mut bytes = raw_non_utf8_name();
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    bytes[central + 28..central + 30].copy_from_slice(&u16::MAX.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    let npz_error = Tensor::read_npz(&path).unwrap_err();
    assert!(
        matches!(npz_error, candle_core::Error::Zip(_)),
        "NPZ must propagate archive metadata failure as Zip, got {npz_error:?}"
    );
    let pickle_error = candle_core::pickle::read_pth_tensor_info(&path, false, None).unwrap_err();
    assert!(
        matches!(pickle_error, candle_core::Error::Zip(_)),
        "pickle must propagate archive metadata failure as Zip, got {pickle_error:?}"
    );
}
#[test]
fn zip9_non_utf8_filename_retains_upstream_cp437_fallback() {
    let mut archive = zip::ZipArchive::new(Cursor::new(raw_non_utf8_name())).unwrap();
    // CP437 0xff is NBSP, not an invalid filename. Test both zip9 entry APIs
    // used by Candle so a future patch cannot turn this into lossy omission.
    let names = archive
        .file_names()
        .map(|name| name.map(|name| name.into_owned()))
        .collect::<zip::result::ZipResult<Vec<String>>>()
        .unwrap();
    assert_eq!(names, vec!["\u{00a0}ata.pkl"]);
    assert_eq!(
        archive.by_index(0).unwrap().name().unwrap().as_ref(),
        "\u{00a0}ata.pkl"
    );
}
#[test]
fn upstream_pickle_tensor_fixture_remains_supported() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test.pt");
    let tensors = candle_core::pickle::PthTensors::new(path, None).unwrap();
    assert!(tensors.get("test").unwrap().is_some());
}
