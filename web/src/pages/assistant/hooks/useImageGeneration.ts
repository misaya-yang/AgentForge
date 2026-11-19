import { useState, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  generateImage,
  getArtifactDownloadUrl,
  createArtifact,
  getSessionArtifacts,
} from "@/api/assistant";
import { addSessionMessage } from "@/api/sessions";
import type { SessionConfig, SessionSummary } from "@/api/sessions";
import { generateUUID } from "@/lib/utils";
import type { ChatMessage } from "../types";
import type { Artifact } from "@/components/artifacts";
import { toast } from "@/hooks/use-toast";
import { imageFailureReceipt } from "../assistantOutcome";

export function useImageGeneration(
  activeSessionId: string | null,
  selectedModel: string,
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  setArtifacts: React.Dispatch<React.SetStateAction<Artifact[]>>,
  setActiveSessionId: (id: string) => void,
  createSession: (params?: {
    service_id?: string;
    metadata?: Record<string, unknown>;
    config?: SessionConfig;
  }) => Promise<{ session_id: string }>,
  listSessions: (params?: { service_id?: string; limit?: number }) => Promise<SessionSummary[]>,
  setSessions: (sessions: SessionSummary[]) => void,
  config: SessionConfig
) {
  const { t } = useTranslation();
  const [isImageMode, setIsImageMode] = useState(false);
  const [isGeneratingImage, setIsGeneratingImage] = useState(false);
  const imageGenerationInFlightRef = useRef(false);

  const handleImageGenerate = useCallback(() => {
    setIsImageMode(true);
  }, []);

  const cancelImageMode = useCallback(() => {
    setIsImageMode(false);
  }, []);

  const sendImageGeneration = useCallback(async (
    prompt: string,
    style: string,
    onSessionBound?: (sessionId: string) => void,
    isOriginCurrent: () => boolean = () => true,
  ): Promise<boolean> => {
    if (
      !prompt.trim() ||
      isGeneratingImage ||
      imageGenerationInFlightRef.current ||
      !selectedModel
    ) return false;

    imageGenerationInFlightRef.current = true;
    setIsGeneratingImage(true);
    setIsImageMode(false);

    // Create session if needed (Reuse logic from chat session ideally, but duplicated here for decoupling)
    let sessionId = activeSessionId;
    if (!sessionId) {
      try {
        const { session_id } = await createSession({
          service_id: "__builtin_assistant__",  // 保留的 service_id
          metadata: { title: `🎨 ${prompt.slice(0, 40)}...` },
          config: {
            selected_model: selectedModel,
            selected_style: style,
            ...config
          },
        });
        sessionId = session_id;
        if (sessionId && isOriginCurrent()) {
          onSessionBound?.(sessionId);
          setActiveSessionId(sessionId);
        }
        const updatedSessions = await listSessions({ service_id: "__builtin_assistant__", limit: 100 })
          .catch(() => null);
        if (updatedSessions && isOriginCurrent()) setSessions(updatedSessions);
      } catch (error) {
        console.error("Failed to create session:", error);
        if (isOriginCurrent()) {
          toast.error(t("assistant.imageSessionUnavailable", "Could not create a conversation for this image. Try again."));
          setIsImageMode(true);
        }
        imageGenerationInFlightRef.current = false;
        setIsGeneratingImage(false);
        return false;
      }
    }
    if (!sessionId) {
      imageGenerationInFlightRef.current = false;
      setIsGeneratingImage(false);
      if (isOriginCurrent()) setIsImageMode(true);
      return false;
    }

    const userMessage: ChatMessage = {
      id: generateUUID(),
      role: "user",
      content: `🎨 ${t("assistant.generateImagePrompt", "Generate image")}: ${prompt}`,
    };

    const assistantMessage: ChatMessage = {
      id: generateUUID(),
      role: "assistant",
      content: "",
      isGeneratingImage: true,
      imageGenerationPrompt: prompt,
    };

    if (isOriginCurrent()) setMessages((prev) =>
      isOriginCurrent() ? [...prev, userMessage, assistantMessage] : prev
    );

    // Save user message
    let providerDispatched = false;
    try {
      await addSessionMessage(sessionId, { role: "user", content: userMessage.content });
      providerDispatched = true;
      const result = await generateImage({
        prompt,
        model_id: selectedModel,
        session_id: sessionId,
        n: 1,
        style: style === "default" ? undefined : style
      });

      if (result.success && result.images.length > 0) {
        const providerName = [result.provider || "Image provider", result.effective_model_id].filter(Boolean).join(" · ");
        const artifactUrls: string[] = [];
        const artifactIds: string[] = [];
        const generatedArtifacts: Array<{ id: string; type: "image"; format: string; title: string; url: string }> = [];

        for (let i = 0; i < result.images.length; i++) {
          const img = result.images[i];
          let artifactId = img.artifact_id;
          if (!artifactId && img.url.startsWith("data:")) {
            try {
              const match = img.url.match(/^data:image\/(\w+);base64,(.+)$/);
              if (match) {
                const format = match[1];
                const base64Data = match[2];
                const imgTitle = `${t("assistant.generatedImage", "Generated Image")} ${i + 1}: ${prompt.slice(0, 30)}...`;
                const artifact = await createArtifact({
                  session_id: sessionId,
                  type: "image",
                  format: format,
                  title: imgTitle,
                  filename: `generated_image_${Date.now()}_${i + 1}.${format}`,
                  content_base64: base64Data,
                  source: "image_generation",
                  metadata: { prompt, provider: result.provider, duration_ms: result.duration_ms },
                });
                artifactId = artifact.artifact_id;
              }
            } catch (error) {
              console.error("Failed to save artifact:", error);
            }
          }
          if (!artifactId) throw new Error("Generated image could not be saved for history");
          const url = getArtifactDownloadUrl(artifactId);
          const imgTitle = `${t("assistant.generatedImage", "Generated Image")} ${i + 1}: ${prompt.slice(0, 30)}...`;
          artifactUrls.push(url);
          artifactIds.push(artifactId);
          generatedArtifacts.push({ id: artifactId, type: "image", format: "png", title: imgTitle, url });
        }

        const savedArtifacts = await getSessionArtifacts(sessionId).catch(() => []);
        const generatedIds = new Set(artifactIds);
        if (isOriginCurrent()) setArtifacts((prev) => {
          if (!isOriginCurrent()) return prev;
          const existingIds = new Set(prev.map((artifact) => artifact.id));
          const added = generatedArtifacts.filter((artifact) => generatedIds.has(artifact.id) && !existingIds.has(artifact.id));
          return [...prev, ...added.map((artifact) => {
            const saved = savedArtifacts.find((item) => item.artifact_id === artifact.id);
            return {
              id: artifact.id,
              type: "image" as const,
              format: saved?.format || artifact.format,
              title: saved?.title || artifact.title,
              url: artifact.url,
              filename: saved?.filename,
              mimeType: saved?.mime_type,
              sizeBytes: saved?.size_bytes,
              source: "ai" as const,
              createdAt: saved?.created_at ? new Date(saved.created_at) : new Date(),
            };
          })];
        });

        const responseContent = artifactUrls.map((url, i) => `![${t("assistant.generatedImage", "Generated Image")} ${i + 1}](${url})`).join("\n\n") +
          `\n\n*${t("assistant.generatedWith", "Generated with")} ${providerName} (${((result.duration_ms || 0) / 1000).toFixed(1)}s)*`;

        if (isOriginCurrent()) setMessages((prev) => !isOriginCurrent() ? prev : prev.map((m) => m.id === assistantMessage.id ? {
          ...m,
          content: responseContent,
          status: "completed",
          isGeneratingImage: false,
          imageGenerationPrompt: undefined,
          generatedArtifacts: generatedArtifacts.length > 0 ? generatedArtifacts : undefined,
        } : m));

        await addSessionMessage(sessionId, {
            role: "assistant",
            content: responseContent,
            metadata: {
              source_kind: "image_generation",
              model_id: result.effective_model_id || undefined,
              stats: { duration_ms: result.duration_ms },
              artifact_ids: artifactIds.length > 0 ? artifactIds : undefined,
            }
          }).catch(() => toast.error(t("assistant.imageHistoryUnavailable", "Could not save the image result in this conversation.")));
        return true;

      } else {
        throw result;
      }
    } catch (error: unknown) {
      const { uncertain, diagnosticId } = imageFailureReceipt(error, providerDispatched);
      const errorContent = uncertain
        ? t("assistant.imageGenerationUnknown", "The image provider's result is unknown. Check this conversation and the image service before trying again.")
        : t("assistant.imageGenerationFailed", "Image generation failed. Check the image provider and try again manually.");
      if (isOriginCurrent()) setMessages((prev) => !isOriginCurrent() ? prev : prev.map((m) => m.id === assistantMessage.id ? {
        ...m, content: errorContent, status: "failed", outcomeUncertain: uncertain,
        diagnosticId, isGeneratingImage: false, imageGenerationPrompt: undefined,
      } : m));

      await addSessionMessage(sessionId, {
        role: "assistant",
        content: errorContent,
        metadata: {
          source_kind: "image_generation",
          process_summary: {
            status: "failed",
            outcome_uncertain: uncertain,
            diagnostic_id: diagnosticId,
            collapsed: true,
            steps: [],
            tools: [],
          },
        },
      }).catch(() => toast.error(t("assistant.imageHistoryUnavailable", "Could not save the image result in this conversation.")));
      return false;
    } finally {
      imageGenerationInFlightRef.current = false;
      setIsGeneratingImage(false);
    }
  }, [isGeneratingImage, selectedModel, activeSessionId, config, t, setMessages, setArtifacts, setActiveSessionId, setSessions, createSession, listSessions]);

  return {
    isImageMode,
    isGeneratingImage,
    handleImageGenerate,
    cancelImageMode,
    sendImageGeneration
  };
}
