//! CPU-native BGE cross-encoder inference, using Candle and the official tokenizer.
//! Model identity is pinned to BAAI's safetensors release; no Python, pickle,
//! custom remote model code, automatic downloads, or lexical fallback is used.
//! References: https://huggingface.co/BAAI/bge-reranker-v2-m3
//! https://github.com/FlagOpen/FlagEmbedding/blob/master/FlagEmbedding/inference/reranker/encoder_only/base.py
use crate::migration_knowledge_query::{
    rerank::LocalRerankBackend,
    types::{QueryError, Result},
};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::xlm_roberta::{Config, XLMRobertaForSequenceClassification};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokenizers::Tokenizer;
pub const MODEL_ID: &str = "BAAI/bge-reranker-v2-m3";
pub const MODEL_REVISION: &str = "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e";
pub const MODEL_SHA256: &str = "d9e3e081faff1eefb84019509b2f5558fd74c1a05a2c7db22f74174fcedb5286";
pub const TOKENIZER_SHA256: &str =
    "69564b696052886ed0ac63fa393e928384e0f8caada38c1f4864a9bfbf379c15";
pub const CONFIG_SHA256: &str = "13dcd6c31d9fec9d1d8e158702072f62d7fa7d312a64b9fe057bec9a08cfe41a";
pub const LARGE_MODEL_ID: &str = "BAAI/bge-reranker-large";
pub const LARGE_MODEL_REVISION: &str = "55611d7bca2a7133960a6d3b71e083071bbfc312";
#[derive(Clone, Copy)]
struct ModelIdentity {
    id: &'static str,
    revision: &'static str,
    weights_hash: &'static str,
    weights_size: u64,
    tokenizer_hash: &'static str,
    tokenizer_size: u64,
    config_hash: &'static str,
    config_size: u64,
    positions: usize,
}
const M3: ModelIdentity = ModelIdentity {
    id: MODEL_ID,
    revision: MODEL_REVISION,
    weights_hash: MODEL_SHA256,
    weights_size: 2_271_071_852,
    tokenizer_hash: TOKENIZER_SHA256,
    tokenizer_size: 17_098_273,
    config_hash: CONFIG_SHA256,
    config_size: 795,
    positions: 8194,
};
const LARGE: ModelIdentity = ModelIdentity {
    id: LARGE_MODEL_ID,
    revision: LARGE_MODEL_REVISION,
    weights_hash: "c5ae4e262c60ed2fb507ec587358285377ec36ee0a2e6da029f0534272b06d36",
    weights_size: 2_239_618_772,
    tokenizer_hash: "9eb652ac4e40cc093272bbbe0f55d521cf67570060227109b5cdc20945a4489e",
    tokenizer_size: 17_098_107,
    config_hash: "25dda0319f57e9de243c32eb7ecefa0768271afac6b5d8a94ba7f779198ab102",
    config_size: 801,
    positions: 514,
};
const MAX_LENGTH: usize = 512;
const QUERY_MAX_LENGTH: usize = MAX_LENGTH * 3 / 4;
fn failure(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
/// The directory contains the unmodified official config.json, tokenizer.json,
/// and model.safetensors at MODEL_REVISION. Provisioning is separate from requests.
#[derive(Clone)]
pub struct NativeBgeReranker {
    directory: PathBuf,
    model_identity: ModelIdentity,
    loaded: Arc<Mutex<Option<Loaded>>>,
}
struct Loaded {
    model: XLMRobertaForSequenceClassification,
    tokenizer: Tokenizer,
}
impl NativeBgeReranker {
    pub fn new(directory: PathBuf) -> Result<Self> {
        Self::for_identity(directory, M3)
    }
    pub fn large(directory: PathBuf) -> Result<Self> {
        Self::for_identity(directory, LARGE)
    }
    fn for_identity(directory: PathBuf, model_identity: ModelIdentity) -> Result<Self> {
        if !directory.is_absolute() {
            return Err(failure("native BGE model directory must be absolute"));
        }
        Ok(Self {
            directory,
            model_identity,
            loaded: Arc::new(Mutex::new(None)),
        })
    }
    fn load(&self) -> Result<Loaded> {
        let identity = self.model_identity;
        verify_file(
            &self.directory.join("config.json"),
            identity.config_hash,
            identity.config_size,
        )?;
        verify_file(
            &self.directory.join("tokenizer.json"),
            identity.tokenizer_hash,
            identity.tokenizer_size,
        )?;
        verify_file(
            &self.directory.join("model.safetensors"),
            identity.weights_hash,
            identity.weights_size,
        )?;
        let raw = std::fs::read(self.directory.join("config.json"))
            .map_err(|_| failure("native BGE configuration unavailable"))?;
        let config: Config = serde_json::from_slice(&raw)
            .map_err(|_| failure("native BGE configuration invalid"))?;
        validate_config(&config, identity.positions)?;
        let mut tokenizer = Tokenizer::from_file(self.directory.join("tokenizer.json"))
            .map_err(|_| failure("native BGE tokenizer unavailable"))?;
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|_| failure("native BGE tokenizer truncation invalid"))?;
        if tokenizer.token_to_id("<s>") != Some(0)
            || tokenizer.token_to_id("</s>") != Some(2)
            || tokenizer.token_to_id("<pad>") != Some(1)
        {
            return Err(failure("native BGE tokenizer identity mismatch"));
        }
        // Managed model artifacts must remain immutable for the model lifetime.
        // Identity and regular-file checks above happen before this trusted mmap.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[self.directory.join("model.safetensors")],
                DType::F32,
                &Device::Cpu,
            )
        }
        .map_err(|_| failure("native BGE weights could not be loaded"))?;
        let model = XLMRobertaForSequenceClassification::new(1, &config, vb)
            .map_err(|_| failure("native BGE classifier shape mismatch"))?;
        Ok(Loaded { model, tokenizer })
    }
    fn compute(&self, query: &str, documents: &[String]) -> Result<Vec<f64>> {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| failure("native BGE model state unavailable"))?;
        if guard.is_none() {
            *guard = Some(self.load()?);
        }
        let loaded = guard
            .as_ref()
            .ok_or_else(|| failure("native BGE initialization failed"))?;
        let query = loaded
            .tokenizer
            .encode(query, false)
            .map_err(|_| failure("native BGE query tokenization failed"))?;
        let q = &query.get_ids()[..query.len().min(QUERY_MAX_LENGTH)];
        let mut out = Vec::with_capacity(documents.len());
        // One pair at a time bounds peak activation memory without changing logits.
        // Source CPU inference disables fp16; the native CPU graph also uses f32.
        for document in documents {
            let document = loaded
                .tokenizer
                .encode(document.as_str(), false)
                .map_err(|_| failure("native BGE document tokenization failed"))?;
            let ids = pair_ids(q, document.get_ids());
            let n = ids.len();
            let ids = Tensor::from_vec(ids, (1, n), &Device::Cpu)
                .map_err(|_| failure("native BGE input tensor failed"))?;
            let attention = Tensor::ones((1, n), DType::U32, &Device::Cpu)
                .map_err(|_| failure("native BGE attention tensor failed"))?;
            let types = Tensor::zeros((1, n), DType::U32, &Device::Cpu)
                .map_err(|_| failure("native BGE type tensor failed"))?;
            let logit = loaded
                .model
                .forward(&ids, &attention, &types)
                .and_then(|v| v.flatten_all())
                .and_then(|v| v.to_vec1::<f32>())
                .map_err(|_| failure("native BGE inference failed"))?;
            if logit.len() != 1 || !logit[0].is_finite() {
                return Err(QueryError::InvalidProviderResponse);
            }
            out.push(sigmoid(logit[0] as f64));
        }
        Ok(out)
    }
}
fn validate_config(c: &Config, positions: usize) -> Result<()> {
    if c.hidden_size != 1024
        || c.num_hidden_layers != 24
        || c.num_attention_heads != 16
        || c.intermediate_size != 4096
        || c.vocab_size != 250002
        || c.pad_token_id != 1
        || c.type_vocab_size != 1
        || c.max_position_embeddings != positions
    {
        return Err(failure(
            "native BGE model architecture does not match the pinned release",
        ));
    }
    Ok(())
}
fn pair_ids(query: &[u32], document: &[u32]) -> Vec<u32> {
    let query = &query[..query.len().min(QUERY_MAX_LENGTH)];
    let document = &document[..document.len().min(MAX_LENGTH - query.len() - 4)];
    let mut out = Vec::with_capacity(query.len() + document.len() + 4);
    out.push(0);
    out.extend_from_slice(query);
    out.extend_from_slice(&[2, 2]);
    out.extend_from_slice(document);
    out.push(2);
    out
}
fn sigmoid(value: f64) -> f64 {
    if value >= 0. {
        1. / (1. + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1. + exp)
    }
}
pub(crate) fn verify_file(path: &Path, expected: &str, size: u64) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| failure("native BGE artifacts are not installed"))?;
    if !metadata.file_type().is_file() || metadata.len() != size {
        return Err(failure("native BGE artifact size or type mismatch"));
    }
    let mut file = File::open(path).map_err(|_| failure("native BGE artifact unavailable"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| failure("native BGE artifact verification failed"))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err(failure(
            "native BGE artifact SHA-256 does not match the official release",
        ));
    }
    Ok(())
}
impl LocalRerankBackend for NativeBgeReranker {
    fn identity(&self) -> String {
        format!(
            "candle-cpu:{}@{}:{}",
            self.model_identity.id, self.model_identity.revision, self.model_identity.weights_hash
        )
    }
    fn scores<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>> {
        Box::pin(async move {
            if provider != "bge" || model != self.model_identity.id {
                return Err(failure(
                    "requested local rerank model is not installed in the native backend",
                ));
            }
            if documents.is_empty() {
                return Ok(vec![]);
            }
            let this = self.clone();
            let query = query.to_owned();
            let documents = documents.to_vec();
            tokio::task::spawn_blocking(move || this.compute(&query, &documents))
                .await
                .map_err(|_| failure("native BGE inference task failed"))?
        })
    }
}
/// Registry for the two source BGE encoder-only model releases. Each model is
/// independently loaded only when requested, and retains its own identity.
#[derive(Clone)]
pub struct NativeLocalReranker {
    pub m3: NativeBgeReranker,
    pub large: NativeBgeReranker,
    pub cross_encoder: crate::native_cross_encoder::NativeCrossEncoder,
}
impl LocalRerankBackend for NativeLocalReranker {
    fn identity(&self) -> String {
        format!(
            "{};{};{}",
            self.m3.identity(),
            self.large.identity(),
            self.cross_encoder.identity()
        )
    }
    fn scores<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>> {
        if provider == "local" {
            self.cross_encoder.scores(provider, model, query, documents)
        } else if model == LARGE_MODEL_ID {
            self.large.scores(provider, model, query, documents)
        } else {
            self.m3.scores(provider, model, query, documents)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flag_pair_truncation_preserves_query_and_special_tokens() {
        let ids = pair_ids(&vec![10; 1000], &vec![20; 1000]);
        assert_eq!(ids.len(), 512);
        assert_eq!(ids[0], 0);
        assert_eq!(&ids[385..387], &[2, 2]);
        assert_eq!(ids[511], 2);
        assert_eq!(ids.iter().filter(|v| **v == 10).count(), 384);
    }
    #[test]
    fn normalized_logits_are_stable() {
        assert_eq!(sigmoid(0.), 0.5);
        assert_eq!(sigmoid(-1000.), 0.);
        assert_eq!(sigmoid(1000.), 1.);
    }
    #[test]
    fn incorrect_artifacts_fail_before_model_loading() {
        let path = std::env::temp_dir().join(format!("bge-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"wrong").unwrap();
        assert!(verify_file(&path, CONFIG_SHA256, 5).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn candle_classifier_executes_a_small_real_tensor_graph() {
        let config:Config=serde_json::from_value(serde_json::json!({"hidden_size":8,"layer_norm_eps":0.00001,"attention_probs_dropout_prob":0.1,"hidden_dropout_prob":0.1,"num_attention_heads":2,"position_embedding_type":"absolute","intermediate_size":16,"hidden_act":"gelu","num_hidden_layers":1,"vocab_size":32,"max_position_embeddings":16,"type_vocab_size":1,"pad_token_id":1})).unwrap();
        let model = XLMRobertaForSequenceClassification::new(
            1,
            &config,
            VarBuilder::zeros(DType::F32, &Device::Cpu),
        )
        .unwrap();
        let ids = Tensor::new(&[[0u32, 4, 2, 2, 5, 2]], &Device::Cpu).unwrap();
        let mask = Tensor::ones((1, 6), DType::U32, &Device::Cpu).unwrap();
        let types = Tensor::zeros((1, 6), DType::U32, &Device::Cpu).unwrap();
        let logits = model
            .forward(&ids, &mask, &types)
            .unwrap()
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap();
        assert_eq!(logits, vec![0.]);
    }
    #[tokio::test]
    #[ignore = "requires pre-provisioned official BGE artifacts; never downloads weights"]
    async fn official_model_real_inference() {
        let directory = std::env::var_os("KNOWLEDGE_TEST_BGE_MODEL_DIRECTORY")
            .expect("pre-provisioned model directory");
        let backend = NativeBgeReranker::new(directory.into()).unwrap();
        let scores = backend
            .scores(
                "bge",
                MODEL_ID,
                "What is the capital of France?",
                &[
                    "Paris is the capital of France.".into(),
                    "A screwdriver is a hand tool.".into(),
                ],
            )
            .await
            .unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|s| (0.0..=1.0).contains(s)));
        assert!(scores[0] > scores[1]);
    }
}
