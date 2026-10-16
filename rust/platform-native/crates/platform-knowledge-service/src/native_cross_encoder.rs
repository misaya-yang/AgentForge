//! Native sentence-transformers default CrossEncoder model. The official model
//! explicitly selects Identity activation, so its scores are raw logits.
use crate::migration_knowledge_query::{
    rerank::LocalRerankBackend,
    types::{QueryError, Result},
};
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{Linear, VarBuilder, linear};
use candle_transformers::models::bert::{BertModel, Config};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokenizers::{Tokenizer, TruncationParams};
pub const MODEL_ID: &str = "cross-encoder/ms-marco-MiniLM-L-12-v2";
pub const MODEL_REVISION: &str = "7b0235231ca2674cb8ca8f022859a6eba2b1c968";
const WEIGHTS_HASH: &str = "1ed84b90cdf3518f76ec9bb93a16f97887eea7c4e7ee5dfb03cc297e394fbbc3";
fn error(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
#[derive(Clone)]
pub struct NativeCrossEncoder {
    directory: PathBuf,
    loaded: Arc<Mutex<Option<Loaded>>>,
}
struct Loaded {
    encoder: BertModel,
    pooler: Linear,
    classifier: Linear,
    tokenizer: Tokenizer,
}
impl NativeCrossEncoder {
    pub fn new(directory: PathBuf) -> Result<Self> {
        if !directory.is_absolute() {
            return Err(error("native cross-encoder directory must be absolute"));
        }
        Ok(Self {
            directory,
            loaded: Arc::new(Mutex::new(None)),
        })
    }
    fn load(&self) -> Result<Loaded> {
        use crate::native_rerank::verify_file;
        verify_file(
            &self.directory.join("config.json"),
            "69a745055b0307584d2903ea5cf4e1899254e511834f358be80d049918b144d8",
            791,
        )?;
        verify_file(
            &self.directory.join("tokenizer.json"),
            "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
            711396,
        )?;
        verify_file(
            &self.directory.join("model.safetensors"),
            WEIGHTS_HASH,
            133469020,
        )?;
        let config: Config = serde_json::from_slice(
            &std::fs::read(self.directory.join("config.json"))
                .map_err(|_| error("cross-encoder config unavailable"))?,
        )
        .map_err(|_| error("cross-encoder config invalid"))?;
        let mut tokenizer = Tokenizer::from_file(self.directory.join("tokenizer.json"))
            .map_err(|_| error("cross-encoder tokenizer invalid"))?;
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|_| error("cross-encoder truncation invalid"))?;
        // Only the verified, immutable safetensors release is mapped; no pickle.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[self.directory.join("model.safetensors")],
                DType::F32,
                &Device::Cpu,
            )
        }
        .map_err(|_| error("cross-encoder weights invalid"))?;
        let encoder = BertModel::load(vb.pp("bert"), &config)
            .map_err(|_| error("cross-encoder shape invalid"))?;
        let pooler = linear(
            config.hidden_size,
            config.hidden_size,
            vb.pp("bert.pooler.dense"),
        )
        .map_err(|_| error("cross-encoder pooler invalid"))?;
        let classifier = linear(config.hidden_size, 1, vb.pp("classifier"))
            .map_err(|_| error("cross-encoder classifier invalid"))?;
        Ok(Loaded {
            encoder,
            pooler,
            classifier,
            tokenizer,
        })
    }
    fn compute(&self, query: &str, documents: &[String]) -> Result<Vec<f64>> {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| error("cross-encoder state unavailable"))?;
        if guard.is_none() {
            *guard = Some(self.load()?);
        }
        let loaded = guard
            .as_ref()
            .ok_or_else(|| error("cross-encoder initialization failed"))?;
        documents
            .iter()
            .map(|document| {
                let pair = loaded
                    .tokenizer
                    .encode((query.trim(), document.trim()), true)
                    .map_err(|_| error("cross-encoder tokenization failed"))?;
                let n = pair.len();
                let inference = || -> candle_core::Result<f64> {
                    let ids = Tensor::from_slice(pair.get_ids(), (1, n), &Device::Cpu)?;
                    let types = Tensor::from_slice(pair.get_type_ids(), (1, n), &Device::Cpu)?;
                    let mask = Tensor::from_slice(pair.get_attention_mask(), (1, n), &Device::Cpu)?;
                    let hidden = loaded.encoder.forward(&ids, &types, Some(&mask))?;
                    let cls = hidden.narrow(1, 0, 1)?.squeeze(1)?;
                    let pooled = loaded.pooler.forward(&cls)?.tanh()?;
                    let score = loaded
                        .classifier
                        .forward(&pooled)?
                        .flatten_all()?
                        .to_vec1::<f32>()?;
                    Ok(score[0] as f64)
                };
                let score = inference().map_err(|_| error("cross-encoder inference failed"))?;
                if !score.is_finite() {
                    return Err(QueryError::InvalidProviderResponse);
                }
                Ok(score)
            })
            .collect()
    }
}
impl LocalRerankBackend for NativeCrossEncoder {
    fn identity(&self) -> String {
        format!("candle-cpu:{MODEL_ID}@{MODEL_REVISION}:{WEIGHTS_HASH}")
    }
    fn scores<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>> {
        Box::pin(async move {
            if provider != "local" || model != MODEL_ID {
                return Err(error("requested native cross-encoder model is unavailable"));
            }
            if documents.is_empty() {
                return Ok(vec![]);
            }
            let this = self.clone();
            let query = query.to_owned();
            let documents = documents.to_vec();
            tokio::task::spawn_blocking(move || this.compute(&query, &documents))
                .await
                .map_err(|_| error("cross-encoder task failed"))?
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bert_pooler_classifier_executes_real_tensors() {
        let config = Config {
            hidden_size: 8,
            num_hidden_layers: 1,
            num_attention_heads: 2,
            intermediate_size: 16,
            vocab_size: 32,
            max_position_embeddings: 16,
            ..Config::default()
        };
        let vb = VarBuilder::zeros(DType::F32, &Device::Cpu);
        let encoder = BertModel::load(vb.pp("bert"), &config).unwrap();
        let pooler = linear(8, 8, vb.pp("bert.pooler.dense")).unwrap();
        let classifier = linear(8, 1, vb.pp("classifier")).unwrap();
        let ids = Tensor::new(&[[2u32, 4, 3, 5, 3]], &Device::Cpu).unwrap();
        let types = Tensor::new(&[[0u32, 0, 0, 1, 1]], &Device::Cpu).unwrap();
        let mask = Tensor::ones((1, 5), DType::U32, &Device::Cpu).unwrap();
        let hidden = encoder.forward(&ids, &types, Some(&mask)).unwrap();
        let cls = hidden.narrow(1, 0, 1).unwrap().squeeze(1).unwrap();
        let pooled = pooler.forward(&cls).unwrap().tanh().unwrap();
        let logits = classifier
            .forward(&pooled)
            .unwrap()
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap();
        assert_eq!(logits, vec![0.]);
    }
    #[tokio::test]
    async fn empty_input_never_loads_artifacts() {
        let model = NativeCrossEncoder::new(std::env::temp_dir()).unwrap();
        assert!(
            model
                .scores("local", MODEL_ID, "query", &[])
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn missing_model_fails_without_fabricated_scores() {
        let model =
            NativeCrossEncoder::new(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()))
                .unwrap();
        assert!(
            model
                .scores("local", MODEL_ID, "query", &["doc".into()])
                .await
                .is_err()
        );
    }
}
