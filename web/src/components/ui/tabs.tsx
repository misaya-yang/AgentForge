import * as React from "react";
import { Tabs as AntTabs } from "antd";
import { elements } from "./composition";

function TabsList(_props: React.HTMLAttributes<HTMLDivElement>) { void _props; return null; }
function TabsTrigger(_props: React.ButtonHTMLAttributes<HTMLButtonElement> & { value: string }) { void _props; return null; }
function TabsContent({ children, value: _value, forceMount: _forceMount, ...props }: React.HTMLAttributes<HTMLDivElement> & { value: string; forceMount?: boolean }) {
  void _value; void _forceMount;
  return <div {...props}>{children}</div>;
}
function Tabs({ children, value, defaultValue, onValueChange, orientation = "horizontal", dir, activationMode: _activationMode, ...props }: React.HTMLAttributes<HTMLDivElement> & {
  value?: string; defaultValue?: string; onValueChange?: (value: string) => void; orientation?: "horizontal" | "vertical"; activationMode?: "automatic" | "manual";
}) {
  void _activationMode;
  const nodes = elements(children);
  const findList = (items: ReturnType<typeof elements>): ReturnType<typeof elements>[number] | undefined => {
    for (const item of items) {
      if (item.type === TabsList) return item;
      if (typeof item.type === "string") {
        const found = findList(elements(item.props.children as React.ReactNode));
        if (found) return found;
      }
    }
  };
  const list = findList(nodes);
  const panels = nodes.filter(node => node.type === TabsContent);
  const items = elements(list?.props.children as React.ReactNode).filter(node => node.type === TabsTrigger).map(node => ({
    key: node.props.value as string,
    label: <span className={node.props.className as string}>{node.props.children as React.ReactNode}</span>,
    disabled: node.props.disabled as boolean | undefined,
    forceRender: panels.find(panel => panel.props.value === node.props.value)?.props.forceMount as boolean | undefined,
    destroyOnHidden: !panels.find(panel => panel.props.value === node.props.value)?.props.forceMount,
    children: panels.find(panel => panel.props.value === node.props.value),
  }));
  const navigation = <AntTabs activeKey={value} defaultActiveKey={defaultValue} onChange={onValueChange}
    tabPosition={orientation === "vertical" ? "left" : "top"} items={items} />;
  const renderLayout = (children: React.ReactNode): React.ReactNode => React.Children.map(children, (child, index) => {
    if (!React.isValidElement<Record<string, unknown>>(child)) return child;
    const node = child;
    if (node.type === TabsList) return <React.Fragment key={index}>{navigation}</React.Fragment>;
    if (node.type === TabsContent) return null;
    if (typeof node.type === "string" || node.type === React.Fragment) return React.cloneElement(node, { key: node.key ?? index }, renderLayout(node.props.children as React.ReactNode));
    return node;
  });
  return <div {...props} dir={dir}>{list ? renderLayout(children) : navigation}</div>;
}
export { Tabs, TabsList, TabsTrigger, TabsContent };
