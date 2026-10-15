/**
 * Segment List - Alibaba Cloud Style Full-Width Layout
 * 
 * Features:
 * - Single column full-width cards (单列横向展开)
 * - Automatic hierarchical structure detection
 * - Clean statistics header
 * - Support for both hierarchical and flat modes
 */

import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { 
  FileText, 
  Layers, 
  LayoutList,
  Hash
} from "lucide-react";
import { cn } from "@/lib/utils";
import type { Segment } from "@/types/knowledge";
import { ChunkCard } from "./ChunkCard";

interface Chunk {
  segment_id: string;
  text: string;
  position: number;
  level?: number;
  parent_segment_id?: string;
  summary?: string;
  /** Segments are enabled unless the backend explicitly stamped them off. */
  enabled?: boolean;
  /** Retrieval telemetry (PRD §5-#16): written by backend T2, shown if present. */
  hit_count?: number;
  children: Chunk[];
}

interface SegmentListProps {
  segments: Segment[];
  /** A partial offset page cannot safely reconstruct parent/child trees. */
  partialPage?: boolean;
  onEdit?: (segmentId: string) => void;
  onDelete?: (segmentId: string) => void;
  onToggleEnabled?: (segmentId: string, enabled: boolean) => void;
  /** Segments with an in-flight mutation render their controls disabled. */
  busySegmentIds?: ReadonlySet<string>;
  batchMode?: boolean;
  selectedSegmentIds?: ReadonlySet<string>;
  onToggleSelect?: (segmentId: string) => void;
}

// Build hierarchical structure from flat segments
function buildHierarchy(segments: Segment[]): Chunk[] {
  // Check if we have hierarchical data
  const hasHierarchy = segments.some(
    s => (s.level !== undefined && s.level !== 3) || s.parent_segment_id
  );
  
  if (!hasHierarchy) {
    // Return as flat list
    return segments.map(s => ({
      segment_id: s.segment_id,
      text: s.text,
      position: s.position,
      level: s.level,
      parent_segment_id: s.parent_segment_id,
      summary: s.summary,
      enabled: s.enabled,
      hit_count: s.hit_count,
      children: []
    }));
  }
  
  // Build parent map
  const parentMap = new Map<string, Chunk>();
  const childrenMap = new Map<string, Chunk[]>();
  
  // First pass: create all chunks
  segments.forEach(seg => {
    const chunk: Chunk = {
      segment_id: seg.segment_id,
      text: seg.text,
      position: seg.position,
      level: seg.level,
      parent_segment_id: seg.parent_segment_id,
      summary: seg.summary,
      enabled: seg.enabled,
      hit_count: seg.hit_count,
      children: []
    };
    
    if (seg.level === 2 || (!seg.parent_segment_id && seg.level !== 3)) {
      // This is a parent
      parentMap.set(seg.segment_id, chunk);
    } else if (seg.parent_segment_id) {
      // This is a child
      const siblings = childrenMap.get(seg.parent_segment_id) || [];
      siblings.push(chunk);
      childrenMap.set(seg.parent_segment_id, siblings);
    } else {
      // Orphan - treat as parent
      parentMap.set(seg.segment_id, chunk);
    }
  });
  
  // Second pass: assign children to parents
  parentMap.forEach((parent, id) => {
    const children = childrenMap.get(id);
    if (children) {
      parent.children = children.sort((a, b) => a.position - b.position);
    }
  });
  
  // Convert to array and sort
  const result = Array.from(parentMap.values());
  result.sort((a, b) => a.position - b.position);
  
  return result;
}

// Calculate statistics
function calculateStats(chunks: Chunk[]) {
  let totalParents = 0;
  let totalChildren = 0;
  let totalChars = 0;
  let disabledCount = 0;

  chunks.forEach(chunk => {
    totalChars += chunk.text?.length || 0;
    if (chunk.enabled === false) disabledCount++;
    if (chunk.children.length > 0) {
      totalParents++;
      totalChildren += chunk.children.length;
      chunk.children.forEach(child => {
        totalChars += child.text?.length || 0;
        if (child.enabled === false) disabledCount++;
      });
    } else {
      totalParents++;
    }
  });

  return {
    totalParents,
    totalChildren,
    totalChars,
    disabledCount,
    isHierarchical: totalChildren > 0,
  };
}

export function SegmentList({
  segments,
  partialPage = false,
  onEdit,
  onDelete,
  onToggleEnabled,
  busySegmentIds,
  batchMode,
  selectedSegmentIds,
  onToggleSelect,
}: SegmentListProps) {
  const { t } = useTranslation();
  const [viewMode, setViewMode] = useState<"hierarchical" | "flat">("hierarchical");

  useEffect(() => {
    if (partialPage) setViewMode("flat");
  }, [partialPage]);
  
  // Build hierarchy
  const hierarchicalChunks = useMemo(() => buildHierarchy(segments), [segments]);
  const flatChunks = useMemo(() => {
    return segments
      .slice()
      .sort((a, b) => a.position - b.position)
      .map(seg => ({
        segment_id: seg.segment_id,
        text: seg.text,
        position: seg.position,
        level: seg.level,
        parent_segment_id: seg.parent_segment_id,
        summary: seg.summary,
        enabled: seg.enabled,
        hit_count: seg.hit_count,
        children: []
      }));
  }, [segments]);
  const displayChunks = partialPage || viewMode === "flat" ? flatChunks : hierarchicalChunks;
  
  // Calculate stats
  const stats = useMemo(() => calculateStats(hierarchicalChunks), [hierarchicalChunks]);
  
  if (segments.length === 0) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        <FileText className="w-12 h-12 mx-auto mb-3 opacity-30" />
        <p>{t("knowledge.detail.noSegments")}</p>
      </div>
    );
  }
  
  return (
    <div className="space-y-4">
      {partialPage && (
        <p className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
          {t("knowledge.segment.partialPageFlatHint")}
        </p>
      )}
      {/* Statistics Header */}
      <div className="flex flex-wrap items-center gap-3 p-3 rounded-xl bg-slate-50 dark:bg-slate-950/50 border border-border">
        <div className="flex items-center gap-2">
          <span className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-white dark:bg-slate-900 border border-border text-sm font-medium">
            <Hash className="w-3.5 h-3.5 text-slate-500" />
            {segments.length} {t("knowledge.segment.totalSegments")}
          </span>
          
          {stats.isHierarchical && (
            <span className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-blue-50 dark:bg-blue-950/30 border border-blue-200 dark:border-blue-900/60 text-sm font-medium text-blue-700 dark:text-blue-300">
              <Layers className="w-3.5 h-3.5" />
              {stats.totalParents} {t("knowledge.segment.parentSegments")} · {stats.totalChildren} {t("knowledge.segment.childSegments")}
            </span>
          )}
          
          <span className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-white dark:bg-slate-900 border border-border text-sm text-muted-foreground">
            {stats.totalChars.toLocaleString()} {t("knowledge.segment.chars")}
            <span className="text-xs text-muted-foreground/60">
              (~{Math.ceil(stats.totalChars / 4)} tokens)
            </span>
          </span>

          {stats.disabledCount > 0 && (
            <span
              data-testid="segment-disabled-count"
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-amber-50 dark:bg-amber-950/30 border border-amber-200 dark:border-amber-900/60 text-sm font-medium text-amber-700 dark:text-amber-300"
            >
              {stats.disabledCount} {t("knowledge.segment.disabledShort")}
            </span>
          )}
        </div>
        
        {stats.isHierarchical && !partialPage && (
          <div className="ml-auto flex items-center gap-1 bg-white dark:bg-slate-900 rounded-lg p-0.5 border border-border">
            <button
              onClick={() => setViewMode("hierarchical")}
              className={cn(
                "px-3 py-1 text-xs font-medium rounded-md transition-colors",
                viewMode === "hierarchical"
                  ? "bg-slate-100 dark:bg-slate-800 text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              )}
            >
              <Layers className="w-3 h-3 inline mr-1" />
              {t("knowledge.segment.hierarchicalView")}
            </button>
            <button
              onClick={() => setViewMode("flat")}
              className={cn(
                "px-3 py-1 text-xs font-medium rounded-md transition-colors",
                viewMode === "flat"
                  ? "bg-slate-100 dark:bg-slate-800 text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              )}
            >
              <LayoutList className="w-3 h-3 inline mr-1" />
              {t("knowledge.segment.flatView")}
            </button>
          </div>
        )}
      </div>
      
      {/* Chunks List - Single Column Full Width */}
      <div className="space-y-3">
        {displayChunks.map((chunk, index) => (
          <ChunkCard
            key={chunk.segment_id}
            chunk={chunk}
            index={index}
            onEdit={onEdit}
            onDelete={onDelete}
            onToggleEnabled={onToggleEnabled}
            busy={busySegmentIds?.has(chunk.segment_id)}
            selectable={batchMode}
            selectedSegmentIds={selectedSegmentIds}
            onToggleSelect={onToggleSelect}
          />
        ))}
      </div>
    </div>
  );
}

export default SegmentList;
