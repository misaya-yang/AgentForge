import type { ThemeConfig } from 'antd';

// AI Gateway operations console — Carbon Indigo palette

export const colors = {
  primary: {
    50: '#f3f3ff', 100: '#e7e7ff', 200: '#d2d2ff', 300: '#b8b8ff',
    400: '#a5a5ff', 500: '#7b7be8', 600: '#5b5bd6', 700: '#4848b8',
    800: '#393994', 900: '#303078',
  },
  neutral: {
    50: '#f7f7f9', 100: '#f0f1f4', 200: '#dcdde2', 300: '#c3c4ca',
    400: '#9598a2', 500: '#62656e', 600: '#4a4c54', 700: '#34353e',
    800: '#202128', 900: '#17181d', 950: '#0f1013',
  },
  gold: {
    50: '#fbf6ec', 100: '#f3e8cf', 200: '#e7d0a3', 300: '#d8b571',
    400: '#c79a4e', 500: '#ae7c32', 600: '#8e6329', 700: '#6f4d23',
    800: '#543b1e', 900: '#3a2a17',
  },
};

const sharedComponents: ThemeConfig['components'] = {
  Button: { borderRadius: 6, primaryShadow: 'none' },
  Card: { borderRadiusLG: 8 },
  Input: { borderRadius: 6 },
  Select: { borderRadius: 6 },
  Table: { borderRadius: 8 },
  Modal: { borderRadiusLG: 12 },
  Tag: { borderRadiusSM: 6 },
};

export const lightTheme: ThemeConfig = {
  token: {
    colorPrimary: '#5b5bd6',
    colorInfo: '#5b5bd6',
    colorSuccess: '#2f8f68',
    colorWarning: '#a86d26',
    colorError: '#d64545',
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Noto Sans SC", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif',
    fontFamilyCode: '"IBM Plex Mono", "JetBrains Mono", "SFMono-Regular", Menlo, Monaco, Consolas, monospace',
    colorBgContainer: '#ffffff',
    colorBgElevated: '#ffffff',
    colorBgLayout: '#f7f7f9',
    colorText: '#17181c',
    colorTextSecondary: '#62656e',
    colorTextTertiary: '#6d707a',
    colorBorder: '#dcdde2',
    colorBorderSecondary: '#e7e8ec',
    borderRadius: 6,
    borderRadiusLG: 8,
    borderRadiusSM: 6,
    borderRadiusXS: 4,
    boxShadow: 'none',
    boxShadowSecondary: '0 1px 2px rgba(0,0,0,0.04)',
  },
  components: {
    ...sharedComponents,
    Menu: {
      itemBg: 'transparent',
      itemSelectedBg: 'rgba(91,91,214,0.10)',
      itemSelectedColor: '#4848b8',
      itemHoverBg: '#f0f1f4',
      itemBorderRadius: 6,
    },
  },
};

export const darkTheme: ThemeConfig = {
  token: {
    colorPrimary: '#a5a5ff',
    colorInfo: '#a5a5ff',
    colorSuccess: '#69c294',
    colorWarning: '#d9aa62',
    colorError: '#f07c78',
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Noto Sans SC", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif',
    fontFamilyCode: '"IBM Plex Mono", "JetBrains Mono", "SFMono-Regular", Menlo, Monaco, Consolas, monospace',
    colorBgContainer: '#17181d',
    colorBgElevated: '#202128',
    colorBgLayout: '#0f1013',
    colorText: '#f4f4f5',
    colorTextSecondary: '#acadb5',
    colorTextTertiary: '#898c96',
    colorBorder: '#34353e',
    colorBorderSecondary: '#292a32',
    borderRadius: 6,
    borderRadiusLG: 8,
    borderRadiusSM: 6,
    borderRadiusXS: 4,
    boxShadow: 'none',
    boxShadowSecondary: 'none',
  },
  components: {
    ...sharedComponents,
    Menu: {
      itemBg: 'transparent',
      itemSelectedBg: 'rgba(165,165,255,0.14)',
      itemSelectedColor: '#c3c3ff',
      itemHoverBg: '#202128',
      itemBorderRadius: 6,
    },
  },
};

export default { lightTheme, darkTheme, colors };
