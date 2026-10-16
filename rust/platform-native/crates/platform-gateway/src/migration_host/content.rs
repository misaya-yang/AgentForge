//! Source content service graph using shared storage and identity authorities.
//! Credentials/configuration and watermark bytes are supplied by the host; this
//! factory does not read environment files, contact providers, or start workers.
use crate::{
    artifact_share_scope::ArtifactShareScope,
    guest_session_manager::GuestSessionManager,
    migration_compat::langgraph::LangGraphClient,
    migration_content::{
        self as content, ContentDatabase, ContentError, ContentServices,
        access::{NativeContentAccess, NativeShareHistory},
        artifacts::{ArtifactApi, ArtifactStorage},
        attachments::{AttachmentBinder, PdfAttachmentParser},
        capabilities::{
            CapabilityAuthorization, ContentCapabilityBroker, PythonCodeCapability,
            SandboxTransport, StorageCodeArtifactSink,
        },
        conversations::ConversationApi,
        files::{CleanupConfig, FileCleanup, FileStorage, FilesApi},
        image_adapters::{AdaptiveImageTransforms, ImageRasterBackend, PinnedImageFetch},
        image_provider::{HttpImageProvider, ImageGenerationConfig},
        images::{ImageService, ImageTaskWorker},
        images_state::ImageRepository,
        read_broker::{ContentReadBroker, TavilySearch},
        shares::ConversationShares,
        skills::{SkillRepository, SkillsApi},
        storage::ObjectStorage,
    },
    quiz_entropy::OsQuizEntropy,
};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

pub struct ContentConfiguration {
    pub uploads_path: PathBuf,
    pub maximum_upload_bytes: usize,
    pub file_admins: BTreeSet<String>,
    pub cleanup: CleanupConfig,
    pub images: ImageGenerationConfig,
    pub watermark_png: Vec<u8>,
}
pub struct ContentDependencies {
    pub database: ContentDatabase,
    pub access: Arc<NativeContentAccess>,
    pub artifacts_backend: Arc<dyn ObjectStorage>,
    pub uploads_backend: Arc<dyn ObjectStorage>,
    pub attachment_fallbacks: Vec<Arc<dyn ObjectStorage>>,
    pub share_sources: Arc<dyn ArtifactShareScope>,
    pub langgraph: Arc<LangGraphClient>,
    pub guests: Arc<GuestSessionManager>,
    pub authorization: CapabilityAuthorization,
    pub sandbox: Option<Arc<dyn SandboxTransport>>,
    pub pdf: Arc<dyn PdfAttachmentParser>,
    pub raster: Arc<dyn ImageRasterBackend>,
    pub search: Arc<TavilySearch>,
}
pub struct AssembledContent {
    pub public: ContentServices,
    pub capabilities: Arc<ContentCapabilityBroker>,
    pub reads: Arc<ContentReadBroker>,
    pub attachments: Arc<AttachmentBinder>,
    pub artifacts: Arc<ArtifactStorage>,
    pub skills: Arc<SkillRepository>,
    pub image_worker: Arc<ImageTaskWorker>,
}
pub fn assemble(
    config: ContentConfiguration,
    dependencies: ContentDependencies,
) -> Result<AssembledContent, ContentError> {
    let d = dependencies;
    let artifacts = Arc::new(ArtifactStorage {
        database: Some(d.database.clone()),
        backend: d.artifacts_backend,
    });
    let provider_model = (!config.images.model.is_empty()).then(|| config.images.model.clone());
    let supports_references = config.images.supports_reference_images;
    let provider = Arc::new(HttpImageProvider::new(config.images)?);
    let images = Arc::new(ImageService {
        repository: ImageRepository {
            database: d.database.clone(),
        },
        artifacts: artifacts.clone(),
        provider: provider.clone(),
        provider_model,
        supports_references,
        fetcher: Arc::new(PinnedImageFetch::default()),
        transforms: Arc::new(AdaptiveImageTransforms::new(
            d.raster,
            &config.watermark_png,
        )?),
        access: d.access.clone(),
    });
    let skills = SkillRepository {
        database: d.database.clone(),
    };
    let public = ContentServices {
        access: d.access.clone(),
        artifacts: Arc::new(ArtifactApi {
            storage: artifacts.clone(),
            access: d.access.clone(),
        }),
        files: Arc::new(FilesApi {
            storage: FileStorage {
                backend: d.uploads_backend.clone(),
            },
            local_path: config.uploads_path,
            max_bytes: config.maximum_upload_bytes,
            admins: config.file_admins,
            cleanup: FileCleanup {
                config: config.cleanup,
            },
            access: d.access.clone(),
        }),
        conversations: Arc::new(ConversationApi {
            access: d.access.clone(),
            langgraph: d.langgraph,
            guests: d.guests,
        }),
        shares: Arc::new(ConversationShares {
            database: d.database.clone(),
            artifacts: artifacts.clone(),
            access: d.access.clone(),
            sources: d.share_sources,
            history: Arc::new(NativeShareHistory {
                access: d.access.clone(),
            }),
            random: Arc::new(OsQuizEntropy),
        }),
        skills: Arc::new(SkillsApi::new(skills.clone(), d.access.clone())),
        images: images.clone(),
    };
    let attachments = Arc::new(AttachmentBinder {
        artifacts: artifacts.clone(),
        uploads: d.uploads_backend,
        local_fallbacks: d.attachment_fallbacks,
        access: d.access,
    });
    let capabilities = Arc::new(ContentCapabilityBroker {
        database: d.database.clone(),
        artifacts: artifacts.clone(),
        authorization: d.authorization.clone(),
        image_provider: provider,
        python: d.sandbox.map(|transport| {
            Arc::new(PythonCodeCapability {
                transport,
                artifacts: Arc::new(StorageCodeArtifactSink {
                    database: d.database,
                    artifacts: artifacts.clone(),
                }),
            })
        }),
        pdf: d.pdf,
    });
    let reads = Arc::new(ContentReadBroker {
        artifacts: artifacts.clone(),
        authorization: d.authorization,
        search: d.search,
    });
    Ok(AssembledContent {
        public,
        capabilities,
        reads,
        attachments,
        artifacts,
        skills: Arc::new(skills),
        image_worker: Arc::new(ImageTaskWorker::new(images)),
    })
}
