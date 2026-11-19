import { useTranslation } from "react-i18next";
import { useAuthStore } from "@/store/useAuthStore";
import { docirCitation } from "@/lib/docirCitation";
export function RegionCitationEvidence({ metadata, datasetId, documentId }: { metadata?: Record<string, unknown>; datasetId?: string; documentId?: string }) {
  const tenant = useAuthStore(state => state.user?.tenant_id), { i18n } = useTranslation();
  const citation = docirCitation(metadata, tenant, datasetId, documentId);
  if (!citation) return null;
  const chinese = i18n.language.startsWith("zh");
  return <details className="my-2 rounded border p-2 text-xs">
    <summary className="cursor-pointer font-medium">{chinese ? "DocIR 区域来源证据" : "DocIR region source evidence"}</summary>
    <dl className="mt-2 grid gap-1 break-all">
      <dt>{chinese ? "页面 / 区块" : "Page / block"}</dt><dd>{citation.page} / {citation.block}</dd>
      <dt>{chinese ? "文档版本 / 发布代际" : "Document version / publication generation"}</dt><dd>{citation.documentVersion} / {citation.generation}</dd>
      <dt>{chinese ? "源文件 SHA-256" : "Source SHA-256"}</dt><dd>{citation.sourceHash}</dd>
      <dt>{chinese ? "坐标单位 / 区域" : "Coordinate space / region"}</dt>
      <dd>{citation.coordinateSpace} · {citation.region ? `${citation.region.x0}, ${citation.region.y0} → ${citation.region.x1}, ${citation.region.y1}` : chinese ? "未提供区域坐标" : "Region coordinates unavailable"}</dd>
    </dl>
    <p className="mt-2 text-muted-foreground">{chinese ? "坐标由服务端从不可变发布版本解析；缺少页面尺寸时不生成伪造的页面覆盖图。" : "Coordinates come from the server's immutable publication. A page overlay requires measured page dimensions."}</p>
  </details>;
}
