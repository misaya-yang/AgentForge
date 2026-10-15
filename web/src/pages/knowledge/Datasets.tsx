import { useState, useMemo } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  Card,
  Input,
  Button,
  Space,
  Tag,
  Typography,
  Row,
  Col,
  Dropdown,
  Modal,
  Tooltip,
  Segmented,
  message,
  Spin,
  Pagination,
} from "antd";
import type { MenuProps } from "antd";
import {
  PlusOutlined,
  SearchOutlined,
  ReloadOutlined,
  DatabaseOutlined,
  FileTextOutlined,
  TableOutlined,
  PictureOutlined,
  PlaySquareOutlined,
  MoreOutlined,
  EditOutlined,
  DeleteOutlined,
  ExperimentOutlined,
  EyeOutlined,
  CopyOutlined,
  CheckOutlined,
  FolderOpenOutlined,
  NodeIndexOutlined,
} from "@ant-design/icons";

import { useDatasets } from "@/hooks/useKnowledge";
import { deleteDataset } from "@/api/knowledge";
import type { Dataset } from "@/types/knowledge";
import { useAppStore } from "@/store/useAppStore";
import { colors } from "@/theme/themeConfig";
import { copyToClipboard } from "@/lib/clipboard";

const { Text, Title, Paragraph } = Typography;
const DATASET_PAGE_SIZE = 24;

// 类型 Tab 配置。多模态项（image / audio_video）暂不提供：后端能力仍是
// text_only（PRD T5-9，L689"要么解锁、要么隐藏滤镜项"），此处选择隐藏
// 滤镜项；卡片上的类型徽章保留真实渲染，已有数据集仍如实标注。
// 后端解锁多模态时，把这两行加回即可。
const typeOptions = [
  { value: "all", labelKey: "knowledge.datasets.typeAll", icon: <DatabaseOutlined /> },
  { value: "document", labelKey: "knowledge.datasets.typeDocument", icon: <FileTextOutlined /> },
  { value: "data", labelKey: "knowledge.datasets.typeData", icon: <TableOutlined /> },
];

// 根据知识库类型获取图标
function getKBTypeIcon(kbType?: string) {
  switch (kbType) {
    case "data":
      return <TableOutlined />;
    case "image":
      return <PictureOutlined />;
    case "audio_video":
      return <PlaySquareOutlined />;
    case "document":
    default:
      return <FileTextOutlined />;
  }
}

// 根据知识库类型获取标签翻译 key
function getKBTypeLabelKey(kbType?: string) {
  switch (kbType) {
    case "data":
      return "knowledge.datasets.typeDataQuery";
    case "image":
      return "knowledge.datasets.typeImageQA";
    case "audio_video":
      return "knowledge.datasets.typeAVSearch";
    case "document":
    default:
      return "knowledge.datasets.typeDocSearch";
  }
}

// 获取知识库类型的颜色
function getKBTypeColor(kbType?: string) {
  void kbType;
  return { bg: colors.primary[50], color: colors.primary[600] };
}

// 知识库卡片组件 - 优化设计
function DatasetCard({
  dataset,
  onViewDetail,
  onHitTest,
  onEdit,
  onDelete,
}: {
  dataset: Dataset;
  index: number;
  onViewDetail: () => void;
  onHitTest: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const { darkMode } = useAppStore();
  const { t } = useTranslation();

  const copyId = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await copyToClipboard(dataset.dataset_id);
      setCopied(true);
      message.success(t("knowledge.datasets.idCopied"));
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      message.error(t("knowledge.datasets.copyFailed", "Failed to copy the knowledge base ID"));
    }
  };

  const menuItems: MenuProps["items"] = [
    {
      key: "view",
      label: t("knowledge.datasets.viewDetail"),
      icon: <EyeOutlined />,
    },
    {
      key: "edit",
      label: t("knowledge.datasets.editConfig"),
      icon: <EditOutlined />,
    },
    {
      key: "test",
      label: t("knowledge.datasets.hitTest"),
      icon: <ExperimentOutlined />,
    },
    { type: "divider" },
    {
      key: "delete",
      label: t("knowledge.datasets.deleteKB"),
      icon: <DeleteOutlined />,
      danger: true,
    },
  ];

  const menuActions: Record<string, () => void> = {
    view: onViewDetail,
    edit: onEdit,
    test: onHitTest,
    delete: onDelete,
  };

  const handleMenuClick: MenuProps["onClick"] = ({ key, domEvent }) => {
    domEvent.stopPropagation();
    menuActions[String(key)]?.();
  };

  const kbTypeColor = getKBTypeColor(dataset.kb_type);
  const docCount = dataset.statistics?.document_count ?? 0;
  const segCount = dataset.statistics?.segment_count ?? 0;

  return (
    <Card
      hoverable
      onClick={onViewDetail}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onViewDetail();
        }
      }}
      role="link"
      tabIndex={0}
      className="border border-border bg-card text-foreground rounded-xl transition-[border-color,box-shadow] hover:border-primary/40 hover:shadow-xs overflow-hidden"
      styles={{
        body: { padding: 0 },
      }}
    >
      <div style={{ padding: "16px 20px" }}>
        {/* 头部：类型标签 + 更多操作 */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginBottom: 12,
          }}
        >
          <Tag
            style={{
              borderRadius: 4,
              padding: "2px 8px",
              fontSize: 11,
              border: "none",
              background: darkMode ? `${kbTypeColor.color}20` : kbTypeColor.bg,
              color: kbTypeColor.color,
              fontWeight: 500,
            }}
          >
            {t(getKBTypeLabelKey(dataset.kb_type))}
          </Tag>

          <Dropdown
            menu={{ items: menuItems, onClick: handleMenuClick }}
            trigger={["click"]}
            placement="bottomRight"
          >
            <button
              type="button"
              aria-label={t("common.actions")}
              onClick={(e) => e.stopPropagation()}
              onMouseDown={(e) => e.stopPropagation()}
              style={{
                width: 28,
                height: 28,
                borderRadius: 6,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                cursor: "pointer",
                background: "transparent",
                border: 0,
                transition: "background 0.2s",
              }}
              onMouseEnter={(e) => {
                e.currentTarget.style.background = darkMode
                  ? colors.neutral[700]
                  : colors.neutral[100];
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.background = "transparent";
              }}
            >
              <MoreOutlined style={{ fontSize: 14, color: colors.neutral[400] }} />
            </button>
          </Dropdown>
        </div>

        {/* 标题和描述 */}
        <div className="mb-3.5">
          <div className="flex items-center gap-2 mb-1">
            <span
              style={{
                fontSize: 18,
                color: kbTypeColor.color,
                display: "flex",
                alignItems: "center",
              }}
            >
              {getKBTypeIcon(dataset.kb_type)}
            </span>
            <h3 className="text-[15px] font-semibold text-foreground truncate flex-1 m-0">
              {dataset.name}
            </h3>
          </div>
          <p className="text-xs text-muted-foreground m-0 min-h-[36px] line-clamp-2 leading-relaxed">
            {dataset.description || t("knowledge.datasets.noDescription")}
          </p>
        </div>

        {/* 统计数据 - 清晰的水平布局 */}
        <div className="flex items-center gap-6 py-2.5 border-y border-border/60">
          <div className="flex items-center gap-2">
            <FileTextOutlined className="text-muted-foreground" style={{ fontSize: 14 }} />
            <div className="flex items-baseline gap-1">
              <span className="text-sm font-bold text-foreground tabular-nums">{docCount}</span>
              <span className="text-[11px] text-muted-foreground">{t("knowledge.datasets.documents")}</span>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <NodeIndexOutlined className="text-muted-foreground" style={{ fontSize: 14 }} />
            <div className="flex items-baseline gap-1">
              <span className="text-sm font-bold text-foreground tabular-nums">{segCount}</span>
              <span className="text-[11px] text-muted-foreground">{t("knowledge.datasets.segments")}</span>
            </div>
          </div>
        </div>

        <div className="mt-2 text-[11px] text-muted-foreground space-y-0.5">
          <p className="m-0 truncate" title={dataset.created_by || dataset.tenant_id || undefined}>
            {t("knowledge.datasets.owner")}: {dataset.created_by || t("knowledge.datasets.ownerUnknown")}
            {dataset.tenant_id ? ` · ${t("knowledge.datasets.tenant")}: ${dataset.tenant_id}` : ""}
          </p>
          <p className="m-0">
            {t("knowledge.datasets.retrieval")}: {dataset.index_config?.retrieval?.mode || "hybrid"}
            {!dataset.index_config?.retrieval ? ` (${t("knowledge.datasets.defaultConfig")})` : ""}
          </p>
        </div>

        {/* 底部：ID + 可见性 */}
        <div className="flex items-center justify-between pt-2.5">
          <div className="flex items-center gap-1">
            <code className="text-[11px] text-muted-foreground font-mono">
              {dataset.dataset_id.slice(0, 10)}...
            </code>
            <Tooltip title={copied ? t("knowledge.datasets.copied") : t("knowledge.datasets.copyId")}>
              <button
                type="button"
                onClick={(event) => void copyId(event)}
                aria-label={copied ? t("knowledge.datasets.copied") : t("knowledge.datasets.copyId")}
                className="cursor-pointer flex p-0.5 border-0 bg-transparent text-muted-foreground hover:text-foreground transition-colors duration-140"
              >
                {copied ? (
                  <CheckOutlined style={{ fontSize: 11, color: colors.primary[500] }} />
                ) : (
                  <CopyOutlined style={{ fontSize: 11 }} />
                )}
              </button>
            </Tooltip>
          </div>
          <span className="rounded px-1.5 py-0.5 text-[10px] font-medium border border-border/50 bg-muted/60 text-muted-foreground">
            {dataset.visibility === "private"
              ? t("knowledge.datasets.visPrivate")
              : dataset.visibility === "tenant"
                ? t("knowledge.datasets.visTenant")
                : t("knowledge.datasets.visPublic")}
          </span>
        </div>
      </div>
    </Card>
  );
}

// 空状态组件
function EmptyState({ onCreateClick }: { onCreateClick: () => void }) {
  const { darkMode } = useAppStore();
  const { t } = useTranslation();

  return (
    <div
      style={{
        textAlign: "center",
        padding: "80px 24px",
      }}
    >
      <div
        style={{
          width: 80,
          height: 80,
          margin: "0 auto 24px",
          borderRadius: 12,
          background: darkMode ? colors.neutral[700] : colors.neutral[100],
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <FolderOpenOutlined
          style={{
            fontSize: 32,
            color: colors.neutral[400],
          }}
        />
      </div>

      <Title level={4} style={{ marginBottom: 6 }}>
        {t("knowledge.datasets.emptyTitle")}
      </Title>
      <Paragraph
        type="secondary"
        style={{
          maxWidth: 480,
          margin: "0 auto 20px",
        }}
      >
        {t("knowledge.datasets.emptyDesc")}
      </Paragraph>

      {/* RAG 拓扑链路指示 */}
      <div
        style={{
          maxWidth: 720,
          margin: "0 auto 28px",
          padding: "16px 20px",
          borderRadius: 10,
          background: darkMode ? "rgba(255,255,255,0.02)" : "rgba(0,0,0,0.02)",
          border: `1px dashed ${darkMode ? colors.neutral[700] : colors.neutral[200]}`,
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(140px, 1fr))",
          gap: 12,
          textAlign: "left",
        }}
      >
        <div style={{ padding: "8px 10px" }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: colors.primary[500], marginBottom: 4 }}>{t("knowledge.datasets.emptyTopology.step1")}</div>
          <div style={{ fontSize: 13, fontWeight: 600 }}>{t("knowledge.datasets.emptyTopology.step1Formats")}</div>
          <div style={{ fontSize: 11, color: colors.neutral[400], marginTop: 2 }}>{t("knowledge.datasets.emptyTopology.step1Desc")}</div>
        </div>
        <div style={{ padding: "8px 10px" }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: colors.primary[500], marginBottom: 4 }}>{t("knowledge.datasets.emptyTopology.step2")}</div>
          <div style={{ fontSize: 13, fontWeight: 600 }}>{t("knowledge.datasets.emptyTopology.step2Formats")}</div>
          <div style={{ fontSize: 11, color: colors.neutral[400], marginTop: 2 }}>{t("knowledge.datasets.emptyTopology.step2Desc")}</div>
        </div>
        <div style={{ padding: "8px 10px" }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: colors.primary[500], marginBottom: 4 }}>{t("knowledge.datasets.emptyTopology.step3")}</div>
          <div style={{ fontSize: 13, fontWeight: 600 }}>{t("knowledge.datasets.emptyTopology.step3Formats")}</div>
          <div style={{ fontSize: 11, color: colors.neutral[400], marginTop: 2 }}>{t("knowledge.datasets.emptyTopology.step3Desc")}</div>
        </div>
        <div style={{ padding: "8px 10px" }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: colors.primary[500], marginBottom: 4 }}>{t("knowledge.datasets.emptyTopology.step4")}</div>
          <div style={{ fontSize: 13, fontWeight: 600 }}>{t("knowledge.datasets.emptyTopology.step4Formats")}</div>
          <div style={{ fontSize: 11, color: colors.neutral[400], marginTop: 2 }}>{t("knowledge.datasets.emptyTopology.step4Desc")}</div>
        </div>
      </div>

      <Space size={12} wrap style={{ justifyContent: "center" }}>
        <Button
          type="primary"
          size="large"
          icon={<PlusOutlined />}
          onClick={onCreateClick}
          style={{
            borderRadius: 6,
            height: 40,
            paddingInline: 24,
          }}
        >
          {t("knowledge.datasets.create")}
        </Button>
      </Space>
    </div>
  );
}

// 无匹配结果（搜索/筛选）状态
function NoResultsState({
  searchQuery,
  typeFilter,
  onResetFilters,
  onCreateClick,
}: {
  searchQuery: string;
  typeFilter: string;
  onResetFilters: () => void;
  onCreateClick: () => void;
}) {
  const { darkMode } = useAppStore();
  const { t } = useTranslation();

  const normalizedQuery = searchQuery.trim();
  const showQuery = normalizedQuery.length > 0;
  const desc = showQuery
    ? t("knowledge.datasets.noResultsDescQuery", { query: normalizedQuery })
    : t("knowledge.datasets.noResultsDesc");

  const typeLabelKey = typeOptions.find((opt) => opt.value === typeFilter)?.labelKey;
  const activeFilters = [
    ...(showQuery ? [normalizedQuery] : []),
    ...(typeFilter !== "all" ? [typeLabelKey ? t(typeLabelKey) : typeFilter] : []),
  ];

  return (
    <div style={{ textAlign: "center", padding: "80px 24px" }}>
      <div
        style={{
          width: 80,
          height: 80,
          margin: "0 auto 24px",
          borderRadius: 12,
          background: darkMode ? colors.neutral[700] : colors.neutral[100],
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <SearchOutlined style={{ fontSize: 32, color: colors.neutral[400] }} />
      </div>

      <Title level={4} style={{ marginBottom: 8 }}>
        {t("knowledge.datasets.noResultsTitle")}
      </Title>

      <Paragraph
        type="secondary"
        style={{ maxWidth: 460, margin: "0 auto 16px" }}
      >
        {desc}
      </Paragraph>

      {activeFilters.length > 0 ? (
        <Space size={8} style={{ marginBottom: 24, flexWrap: "wrap" }}>
          {activeFilters.map((f, idx) => (
            <Tag key={`${idx}-${f}`} style={{ borderRadius: 999 }}>
              {f}
            </Tag>
          ))}
        </Space>
      ) : (
        <div style={{ height: 24 }} />
      )}

      <Space size={12}>
        <Button
          type="primary"
          size="large"
          onClick={onResetFilters}
          style={{ borderRadius: 6, height: 40, paddingInline: 20 }}
        >
          {t("knowledge.datasets.resetFilters")}
        </Button>
        <Button
          size="large"
          icon={<PlusOutlined />}
          onClick={onCreateClick}
          style={{ borderRadius: 6, height: 40, paddingInline: 20 }}
        >
          {t("knowledge.datasets.create")}
        </Button>
      </Space>
    </div>
  );
}

// 主页面组件
export function KnowledgeDatasetsPage() {
  const nav = useNavigate();
  const qc = useQueryClient();
  const datasetsQuery = useDatasets();
  const datasets = useMemo(() => datasetsQuery.data || [], [datasetsQuery.data]);
  const { darkMode } = useAppStore();
  const { t } = useTranslation();

  const [searchQuery, setSearchQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<string>("all");
  const [page, setPage] = useState(1);
  const [deleteModalOpen, setDeleteModalOpen] = useState(false);
  const [deletingDataset, setDeletingDataset] = useState<Dataset | null>(null);
  const [deletePassword, setDeletePassword] = useState("");
  const [deleteSubmitting, setDeleteSubmitting] = useState(false);

  const resetDeleteModal = () => {
    setDeleteModalOpen(false);
    setDeletingDataset(null);
    setDeletePassword("");
  };

  // 过滤后的数据集
  const filteredDatasets = useMemo(() => {
    return datasets.filter((d) => {
      const matchesSearch =
        d.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
        d.dataset_id.toLowerCase().includes(searchQuery.toLowerCase()) ||
        (d.description || "").toLowerCase().includes(searchQuery.toLowerCase()) ||
        (d.created_by || "").toLowerCase().includes(searchQuery.toLowerCase()) ||
        (d.tenant_id || "").toLowerCase().includes(searchQuery.toLowerCase());
      const matchesType =
        typeFilter === "all" || (d.kb_type || "document") === typeFilter;
      return matchesSearch && matchesType;
    });
  }, [datasets, searchQuery, typeFilter]);
  const currentPage = Math.min(page, Math.max(1, Math.ceil(filteredDatasets.length / DATASET_PAGE_SIZE)));
  const pageDatasets = filteredDatasets.slice((currentPage - 1) * DATASET_PAGE_SIZE, currentPage * DATASET_PAGE_SIZE);

  // 统计数据
  const stats = useMemo(() => {
    const totalDocs = datasets.reduce(
      (sum, d) => sum + (d.statistics?.document_count ?? 0),
      0
    );
    const totalSegments = datasets.reduce(
      (sum, d) => sum + (d.statistics?.segment_count ?? 0),
      0
    );
    return {
      total: datasets.length,
      documents: totalDocs,
      segments: totalSegments,
    };
  }, [datasets]);

  // 删除操作
  const handleDelete = async () => {
    if (!deletingDataset || deleteSubmitting) return;
    if (!deletePassword) {
      message.warning(t("knowledge.datasets.deletePasswordRequired"));
      return;
    }
    setDeleteSubmitting(true);
    try {
      await deleteDataset(deletingDataset.dataset_id, {
        password: deletePassword,
      });
      await qc.invalidateQueries({ queryKey: ["kb-datasets"] });
      message.success(t("knowledge.datasets.deleteSuccess"));
      resetDeleteModal();
    } catch (e) {
      message.error(t("knowledge.datasets.deleteFailed") + ": " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setDeleteSubmitting(false);
    }
  };

  return (
    <div className="knowledge-datasets-page ui-page" style={{ maxWidth: 1400, margin: "0 auto", paddingTop: 12 }}>
      {/* 页面标题区 */}
      <div style={{ marginBottom: 12 }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-end",
            gap: 12,
            flexWrap: "wrap",
          }}
        >
          <Space size={12}>
            <Button
              icon={<ReloadOutlined spin={datasetsQuery.isFetching} />}
              onClick={() => qc.invalidateQueries({ queryKey: ["kb-datasets"] })}
              style={{ borderRadius: 6 }}
            >
              {t("knowledge.datasets.refresh")}
            </Button>
            {datasets.length > 0 && (
              <Button
                type="primary"
                icon={<PlusOutlined />}
                onClick={() => nav("/knowledge/create")}
                style={{ borderRadius: 6 }}
              >
                {t("knowledge.datasets.create")}
              </Button>
            )}
          </Space>
        </div>
      </div>

      {/* 统计指标区 - 统一结构化横幅 */}
      <div className="knowledge-stats-strip mb-5 p-4 rounded-xl border border-border bg-card text-foreground grid grid-cols-1 sm:grid-cols-3 gap-4">
        <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
          <div
            style={{
              width: 40,
              height: 40,
              borderRadius: 8,
              background: darkMode ? "rgba(91,91,214,0.14)" : colors.primary[50],
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              flexShrink: 0,
            }}
          >
            <DatabaseOutlined style={{ fontSize: 18, color: colors.primary[500] }} />
          </div>
          <div>
            <Text type="secondary" style={{ fontSize: 12.5, fontWeight: 500 }}>
              {t("knowledge.datasets.totalKB")}
            </Text>
            <div
              className="tabular-nums"
              style={{ fontSize: 24, fontWeight: 700, lineHeight: 1.2, fontVariantNumeric: "tabular-nums" }}
            >
              {stats.total.toLocaleString()}
            </div>
          </div>
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
          <div
            style={{
              width: 40,
              height: 40,
              borderRadius: 8,
              background: darkMode ? "rgba(91,91,214,0.14)" : colors.primary[50],
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              flexShrink: 0,
            }}
          >
            <FileTextOutlined style={{ fontSize: 18, color: colors.primary[400] }} />
          </div>
          <div>
            <Text type="secondary" style={{ fontSize: 12.5, fontWeight: 500 }}>
              {t("knowledge.datasets.totalDocs")}
            </Text>
            <div
              className="tabular-nums"
              style={{ fontSize: 24, fontWeight: 700, lineHeight: 1.2, fontVariantNumeric: "tabular-nums" }}
            >
              {stats.documents.toLocaleString()}
            </div>
          </div>
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
          <div
            style={{
              width: 40,
              height: 40,
              borderRadius: 8,
              background: darkMode ? "rgba(91,91,214,0.14)" : colors.primary[50],
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              flexShrink: 0,
            }}
          >
            <NodeIndexOutlined style={{ fontSize: 18, color: colors.primary[600] }} />
          </div>
          <div>
            <Text type="secondary" style={{ fontSize: 12.5, fontWeight: 500 }}>
              {t("knowledge.datasets.totalSegments")}
            </Text>
            <div
              className="tabular-nums"
              style={{ fontSize: 24, fontWeight: 700, lineHeight: 1.2, fontVariantNumeric: "tabular-nums" }}
            >
              {stats.segments.toLocaleString()}
            </div>
          </div>
        </div>
      </div>

      {/* 搜索和筛选栏 */}
      <Card
        className="mb-6 rounded-xl border border-border bg-card text-foreground"
        styles={{ body: { padding: "12px 16px" } }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            flexWrap: "wrap",
            gap: 16,
          }}
        >
          {/* 左侧：搜索框 */}
          <Input
            className="knowledge-search-input"
            placeholder={t("knowledge.datasets.searchPlaceholder")}
            prefix={<SearchOutlined style={{ color: colors.neutral[400] }} />}
            value={searchQuery}
            onChange={(e) => { setSearchQuery(e.target.value); setPage(1); }}
            allowClear
            style={{
              width: 320,
              borderRadius: 6,
            }}
          />

          {/* 右侧：类型筛选 */}
          <div className="knowledge-type-rail ui-tabs-rail">
            <Segmented
              value={typeFilter}
              onChange={(value) => { setTypeFilter(value as string); setPage(1); }}
              options={typeOptions.map((opt) => ({
                value: opt.value,
                label: (
                  <Space size={6}>
                    {opt.icon}
                    <span>{t(opt.labelKey)}</span>
                  </Space>
                ),
              }))}
              style={{
                background: darkMode
                  ? colors.neutral[700]
                  : colors.neutral[100],
                borderRadius: 6,
                padding: 2,
              }}
            />
          </div>
        </div>
      </Card>

      {/* 知识库列表 */}
      {datasetsQuery.isLoading ? (
        <div style={{ textAlign: "center", padding: "80px 0" }}>
          <Spin size="large" />
          <div style={{ marginTop: 16 }}>
            <Text type="secondary">{t("knowledge.datasets.loading")}</Text>
          </div>
        </div>
      ) : datasetsQuery.isError ? (
        <div className="ui-empty-state" role="alert">
          <Title level={4} style={{ margin: 0 }}>
            {t("knowledge.datasets.loadFailed", "Couldn't load knowledge bases")}
          </Title>
          <Paragraph type="secondary" style={{ margin: 0 }}>
            {t("knowledge.datasets.loadFailedDesc", "Check the connection and try again.")}
          </Paragraph>
          <Button type="primary" onClick={() => void datasetsQuery.refetch()}>
            {t("common.retry", "Retry")}
          </Button>
        </div>
      ) : datasets.length === 0 ? (
        <EmptyState onCreateClick={() => nav("/knowledge/create")} />
      ) : filteredDatasets.length === 0 ? (
        <NoResultsState
          searchQuery={searchQuery}
          typeFilter={typeFilter}
          onResetFilters={() => {
            setSearchQuery("");
            setTypeFilter("all");
            setPage(1);
          }}
          onCreateClick={() => nav("/knowledge/create")}
        />
      ) : (
        <>
        <Row gutter={[16, 16]}>
          {pageDatasets.map((dataset, index) => (
            <Col key={dataset.dataset_id} xs={24} sm={12} lg={8} xl={8}>
              <DatasetCard
                dataset={dataset}
                index={index}
                onViewDetail={() => nav(`/knowledge/${dataset.dataset_id}`)}
                onHitTest={() =>
                  nav(`/knowledge/${dataset.dataset_id}?tab=retrieval`)
                }
                onEdit={() =>
                  nav(`/knowledge/${dataset.dataset_id}?tab=settings`)
                }
                onDelete={() => {
                  setDeletingDataset(dataset);
                  setDeletePassword("");
                  setDeleteModalOpen(true);
                }}
              />
            </Col>
          ))}
        </Row>
        <div className="mt-5 flex justify-end">
          <Pagination
            current={currentPage}
            pageSize={DATASET_PAGE_SIZE}
            total={filteredDatasets.length}
            onChange={setPage}
            showSizeChanger={false}
            showTotal={(total) => t("knowledge.datasets.resultCount", { count: total })}
          />
        </div>
        </>
      )}

      {/* 删除确认弹窗 */}
      <Modal
        title={
          <Space>
            <DeleteOutlined style={{ color: "#EF4444" }} />
            <span>{t("knowledge.datasets.deleteConfirmTitle")}</span>
          </Space>
        }
        open={deleteModalOpen}
        onCancel={() => {
          if (deleteSubmitting) {
            return;
          }
          resetDeleteModal();
        }}
        onOk={handleDelete}
        confirmLoading={deleteSubmitting}
        mask={{ closable: !deleteSubmitting }}
        keyboard={!deleteSubmitting}
        okText={t("knowledge.datasets.deleteConfirm")}
        cancelText={t("common.cancel")}
        okButtonProps={{
          danger: true,
          disabled: !deletePassword || deleteSubmitting,
          loading: deleteSubmitting,
        }}
        cancelButtonProps={{
          disabled: deleteSubmitting,
        }}
      >
        <div style={{ padding: "12px 0" }}>
          <Text>
            {t("knowledge.datasets.deleteConfirmText", { name: deletingDataset?.name })}
          </Text>
          <br />
          <Text type="secondary" style={{ fontSize: 13 }}>
            {t("knowledge.datasets.deleteWarning")}
          </Text>
          <div style={{ marginTop: 12 }}>
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t("knowledge.datasets.deletePasswordLabel")}
            </Text>
            <Input.Password
              value={deletePassword}
              onChange={(e) => setDeletePassword(e.target.value)}
              placeholder={t("knowledge.datasets.deletePasswordPlaceholder")}
              autoComplete="current-password"
              disabled={deleteSubmitting}
              onPressEnter={() => {
                if (!deleteSubmitting && deletePassword) {
                  void handleDelete();
                }
              }}
              style={{ marginTop: 6 }}
            />
          </div>
        </div>
      </Modal>
      <style>{`
        .knowledge-type-rail .ant-segmented-group {
          width: max-content;
        }
        @media (max-width: 640px) {
          .knowledge-datasets-page {
            padding-top: 4px !important;
          }
          .knowledge-stats-row {
            display: grid !important;
            grid-template-columns: repeat(2, minmax(0, 1fr));
            gap: 10px;
            margin-inline: 0 !important;
            margin-bottom: 16px !important;
          }
          .knowledge-stats-row > .ant-col {
            max-width: none !important;
            width: auto !important;
            flex: none !important;
            padding: 0 !important;
          }
          .knowledge-stats-row > .ant-col:first-child {
            grid-column: 1 / -1;
          }
          .knowledge-search-input {
            width: 100% !important;
          }
          .knowledge-type-rail {
            width: 100%;
          }
        }
      `}</style>
    </div>
  );
}
