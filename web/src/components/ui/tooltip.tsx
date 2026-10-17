import * as React from "react";
import { Tooltip as AntTooltip } from "antd";
import { elements } from "./composition";
const DelayContext = React.createContext(700);
function TooltipProvider({ children, delayDuration = 700 }: { children?: React.ReactNode; delayDuration?: number; skipDelayDuration?: number }) {
  return <DelayContext.Provider value={delayDuration}>{children}</DelayContext.Provider>;
}
function TooltipTrigger(_props: React.ButtonHTMLAttributes<HTMLButtonElement> & { asChild?: boolean }) { void _props; return null; }
function TooltipContent(_props: React.HTMLAttributes<HTMLDivElement> & { side?: "top" | "bottom" | "left" | "right"; sideOffset?: number; align?: string }) { void _props; return null; }
function Tooltip({ children, open, defaultOpen, onOpenChange, delayDuration }: { children?: React.ReactNode; open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; delayDuration?: number }) {
  const inheritedDelay = React.useContext(DelayContext);
  const nodes = elements(children);
  const trigger = nodes.find(node => node.type === TooltipTrigger)?.props as React.ComponentProps<typeof TooltipTrigger> | undefined;
  const content = nodes.find(node => node.type === TooltipContent)?.props as React.ComponentProps<typeof TooltipContent> | undefined;
  if (!trigger) return null;
  const { asChild, children: triggerChildren, ...triggerProps } = trigger;
  return <AntTooltip title={content?.children} placement={content?.side ?? "top"} open={open} defaultOpen={defaultOpen} onOpenChange={onOpenChange}
    mouseEnterDelay={(delayDuration ?? inheritedDelay) / 1000} classNames={{ root: content?.className }}>
    {asChild ? triggerChildren : <button type="button" {...triggerProps}>{triggerChildren}</button>}
  </AntTooltip>;
}
export { TooltipProvider, Tooltip, TooltipTrigger, TooltipContent };
