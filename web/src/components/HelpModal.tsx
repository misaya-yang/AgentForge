import { Modal, Typography, Collapse, Space, Tag, Divider } from "antd";
import {
  DashboardOutlined,
  CloudServerOutlined,
  DatabaseOutlined,
  ThunderboltOutlined,
  UnorderedListOutlined,
  SettingOutlined,
  RocketOutlined,
  BookOutlined,
  ApiOutlined,
  BulbOutlined,
} from "@ant-design/icons";
import { colors } from "@/theme/themeConfig";
import { useTranslation } from "react-i18next";

const { Title, Text, Paragraph } = Typography;

interface HelpModalProps {
  open: boolean;
  onClose: () => void;
}

export function HelpModal({ open, onClose }: HelpModalProps) {
  const { t } = useTranslation();
  const features = [
    {
      key: "dashboard",
      label: (
        <Space>
          <DashboardOutlined style={{ color: colors.primary[500] }} />
          <span>{t("help.sections.dashboard.title")}</span>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.dashboard.description")}
          </Paragraph>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li>{t("help.sections.dashboard.bullets.health")}</li>
            <li>{t("help.sections.dashboard.bullets.metrics")}</li>
            <li>{t("help.sections.dashboard.bullets.overview")}</li>
          </ul>
        </div>
      ),
    },
    {
      key: "services",
      label: (
        <Space>
          <CloudServerOutlined style={{ color: colors.primary[400] }} />
          <span>{t("help.sections.services.title")}</span>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.services.description")}
          </Paragraph>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li><strong>{t("help.sections.services.bullets.registerTitle")}</strong>：{t("help.sections.services.bullets.registerDesc")}</li>
            <li><strong>{t("help.sections.services.bullets.configureTitle")}</strong>：{t("help.sections.services.bullets.configureDesc")}</li>
            <li><strong>{t("help.sections.services.bullets.healthTitle")}</strong>：{t("help.sections.services.bullets.healthDesc")}</li>
            <li><strong>{t("help.sections.services.bullets.typesTitle")}</strong>：{t("help.sections.services.bullets.typesDesc")}</li>
          </ul>
        </div>
      ),
    },
    {
      key: "knowledge",
      label: (
        <Space>
          <DatabaseOutlined style={{ color: colors.primary[500] }} />
          <span>{t("help.sections.knowledge.title")}</span>
          <Tag color="blue" style={{ marginLeft: 4 }}>{t("help.sections.knowledge.coreTag")}</Tag>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.knowledge.description")}
          </Paragraph>
          <Title level={5} style={{ marginTop: 12 }}>{t("help.sections.knowledge.featuresTitle")}</Title>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li><strong>{t("help.sections.knowledge.features.createTitle")}</strong>：{t("help.sections.knowledge.features.createDesc")}</li>
            <li><strong>{t("help.sections.knowledge.features.uploadTitle")}</strong>：{t("help.sections.knowledge.features.uploadDesc")}</li>
            <li><strong>{t("help.sections.knowledge.features.urlTitle")}</strong>：{t("help.sections.knowledge.features.urlDesc")}</li>
            <li><strong>{t("help.sections.knowledge.features.segmentTitle")}</strong>：{t("help.sections.knowledge.features.segmentDesc")}</li>
            <li><strong>{t("help.sections.knowledge.features.hitTitle")}</strong>：{t("help.sections.knowledge.features.hitDesc")}</li>
            <li><strong>{t("help.sections.knowledge.features.qaTitle")}</strong>：{t("help.sections.knowledge.features.qaDesc")}</li>
          </ul>
          <Title level={5} style={{ marginTop: 12 }}>{t("help.sections.knowledge.modesTitle")}</Title>
          <Space wrap style={{ marginTop: 8 }}>
            <Tag>{t("help.sections.knowledge.modes.vector")}</Tag>
            <Tag>{t("help.sections.knowledge.modes.keyword")}</Tag>
            <Tag>{t("help.sections.knowledge.modes.hybrid")}</Tag>
            <Tag>BM25</Tag>
            <Tag>{t("help.sections.knowledge.modes.rerank")}</Tag>
          </Space>
        </div>
      ),
    },
    {
      key: "playground",
      label: (
        <Space>
          <ThunderboltOutlined style={{ color: colors.primary[600] }} />
          <span>{t("help.sections.playground.title")}</span>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.playground.description")}
          </Paragraph>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li><strong>{t("help.sections.playground.bullets.multiTurnTitle")}</strong>：{t("help.sections.playground.bullets.multiTurnDesc")}</li>
            <li><strong>{t("help.sections.playground.bullets.streamingTitle")}</strong>：{t("help.sections.playground.bullets.streamingDesc")}</li>
            <li><strong>{t("help.sections.playground.bullets.sessionsTitle")}</strong>：{t("help.sections.playground.bullets.sessionsDesc")}</li>
            <li><strong>{t("help.sections.playground.bullets.toolsTitle")}</strong>：{t("help.sections.playground.bullets.toolsDesc")}</li>
            <li><strong>{t("help.sections.playground.bullets.multimodalTitle")}</strong>：{t("help.sections.playground.bullets.multimodalDesc")}</li>
          </ul>
        </div>
      ),
    },
    {
      key: "tasks",
      label: (
        <Space>
          <UnorderedListOutlined style={{ color: colors.primary[400] }} />
          <span>{t("help.sections.tasks.title")}</span>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.tasks.description")}
          </Paragraph>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li>{t("help.sections.tasks.bullets.queryStatus")}</li>
            <li>{t("help.sections.tasks.bullets.results")}</li>
            <li>{t("help.sections.tasks.bullets.longRunning")}</li>
          </ul>
        </div>
      ),
    },
    {
      key: "settings",
      label: (
        <Space>
          <SettingOutlined style={{ color: colors.neutral[500] }} />
          <span>{t("help.sections.settings.title")}</span>
        </Space>
      ),
      children: (
        <div>
          <Paragraph>
            {t("help.sections.settings.description")}
          </Paragraph>
          <ul style={{ paddingLeft: 20, margin: "8px 0" }}>
            <li>{t("help.sections.settings.bullets.llm")}</li>
            <li>{t("help.sections.settings.bullets.embedding")}</li>
            <li>{t("help.sections.settings.bullets.system")}</li>
          </ul>
        </div>
      ),
    },
  ];

  return (
    <Modal
      title={
        <Space>
          <BookOutlined style={{ color: colors.primary[500] }} />
          <span>{t("help.title")}</span>
        </Space>
      }
      open={open}
      onCancel={onClose}
      footer={null}
      width={680}
      styles={{
        body: { maxHeight: "70vh", overflow: "auto" },
      }}
    >
      {/* 项目介绍 */}
      <div style={{
        padding: "16px 20px",
        background: `linear-gradient(135deg, ${colors.primary[500]}10, ${colors.primary[300]}10)`,
        borderRadius: 12,
        marginBottom: 20,
      }}>
        <Space align="start">
          <RocketOutlined style={{ fontSize: 32, color: colors.primary[500], marginTop: 4 }} />
          <div>
            <Title level={4} style={{ margin: 0 }}>{t("help.platformName")}</Title>
            <Text type="secondary">{t("help.description")}</Text>
            <Paragraph style={{ marginTop: 8, marginBottom: 0 }}>
              {t("help.intro")}
            </Paragraph>
          </div>
        </Space>
      </div>

      {/* 快速开始 */}
      <div style={{ marginBottom: 20 }}>
        <Title level={5}>
          <BulbOutlined style={{ marginRight: 8, color: colors.primary[500] }} />
          {t("help.quickStart.title")}
        </Title>
        <div style={{
          display: "grid",
          gridTemplateColumns: "repeat(2, 1fr)",
          gap: 12,
        }}>
          <div style={{
            padding: "12px 16px",
            background: colors.neutral[50],
            borderRadius: 8,
            border: `1px solid ${colors.neutral[200]}`,
          }}>
            <Text strong>{t("help.quickStart.steps.register.title")}</Text>
            <br />
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t("help.quickStart.steps.register.desc")}
            </Text>
          </div>
          <div style={{
            padding: "12px 16px",
            background: colors.neutral[50],
            borderRadius: 8,
            border: `1px solid ${colors.neutral[200]}`,
          }}>
            <Text strong>{t("help.quickStart.steps.knowledge.title")}</Text>
            <br />
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t("help.quickStart.steps.knowledge.desc")}
            </Text>
          </div>
          <div style={{
            padding: "12px 16px",
            background: colors.neutral[50],
            borderRadius: 8,
            border: `1px solid ${colors.neutral[200]}`,
          }}>
            <Text strong>{t("help.quickStart.steps.chat.title")}</Text>
            <br />
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t("help.quickStart.steps.chat.desc")}
            </Text>
          </div>
          <div style={{
            padding: "12px 16px",
            background: colors.neutral[50],
            borderRadius: 8,
            border: `1px solid ${colors.neutral[200]}`,
          }}>
            <Text strong>{t("help.quickStart.steps.integrate.title")}</Text>
            <br />
            <Text type="secondary" style={{ fontSize: 12 }}>
              {t("help.quickStart.steps.integrate.desc")}
            </Text>
          </div>
        </div>
      </div>

      <Divider style={{ margin: "16px 0" }} />

      {/* 功能模块 */}
      <Title level={5}>
        <ApiOutlined style={{ marginRight: 8, color: colors.primary[500] }} />
        {t("help.sectionsTitle")}
      </Title>
      <Collapse
        items={features}
        defaultActiveKey={["knowledge"]}
        style={{ background: "transparent" }}
        bordered={false}
      />

      {/* 技术支持 */}
      <div style={{
        marginTop: 20,
        padding: "12px 16px",
        background: colors.neutral[50],
        borderRadius: 8,
        textAlign: "center",
      }}>
        <Text type="secondary">
          {t("help.support")}
        </Text>
      </div>
    </Modal>
  );
}
