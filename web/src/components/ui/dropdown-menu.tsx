import * as React from "react";
import { Dropdown, Menu } from "antd";
import type { MenuProps } from "antd";
import { Check, Circle } from "lucide-react";
import { elements } from "./composition";
import { cn } from "@/lib/utils";

type ItemProps = Omit<React.HTMLAttributes<HTMLDivElement>, "onSelect"> & {
  disabled?: boolean; inset?: boolean; asChild?: boolean; onSelect?: (event: Event) => void;
};
function DropdownMenuTrigger(_props: React.ButtonHTMLAttributes<HTMLButtonElement> & { asChild?: boolean }) { void _props; return null; }
function DropdownMenuContent(_props: React.HTMLAttributes<HTMLDivElement> & { align?: "start" | "center" | "end"; side?: "top" | "bottom" | "left" | "right"; sideOffset?: number; alignOffset?: number }) { void _props; return null; }
function DropdownMenuItem(_props: ItemProps) { void _props; return null; }
function DropdownMenuCheckboxItem(_props: ItemProps & { checked?: boolean | "indeterminate"; onCheckedChange?: (checked: boolean) => void }) { void _props; return null; }
function DropdownMenuRadioItem(_props: ItemProps & { value: string }) { void _props; return null; }
function DropdownMenuRadioGroup(_props: { children?: React.ReactNode; value?: string; onValueChange?: (value: string) => void }) { void _props; return null; }
function DropdownMenuLabel(_props: React.HTMLAttributes<HTMLDivElement> & { inset?: boolean }) { void _props; return null; }
function DropdownMenuSeparator(_props: React.HTMLAttributes<HTMLDivElement>) { void _props; return null; }
function DropdownMenuGroup(_props: { children?: React.ReactNode }) { void _props; return null; }
function DropdownMenuPortal({ children }: { children?: React.ReactNode }) { return <>{children}</>; }
function DropdownMenuSub(_props: { children?: React.ReactNode }) { void _props; return null; }
const DropdownMenuSubTrigger = DropdownMenuItem;
const DropdownMenuSubContent = DropdownMenuContent;
function DropdownMenuShortcut({ className, ...props }: React.HTMLAttributes<HTMLSpanElement>) { return <span {...props} className={cn("ui-dropdown-menu-slot-1", className)} />; }

type Radio = React.ComponentProps<typeof DropdownMenuRadioGroup>;
function menuItems(children: React.ReactNode, close: () => void, path = "menu", radio?: Radio): NonNullable<MenuProps["items"]> {
  return elements(children).flatMap((node, index): NonNullable<MenuProps["items"]> => {
    const key = `${path}-${index}`;
    const props = node.props;
    if (node.type === DropdownMenuSeparator) return [{ key, type: "divider" }];
    if (node.type === DropdownMenuRadioGroup) return menuItems(props.children as React.ReactNode, close, key, props as Radio);
    if (node.type === DropdownMenuSub) {
      const sub = elements(props.children as React.ReactNode);
      return [{ key, label: sub.find(child => child.type === DropdownMenuSubTrigger)?.props.children as React.ReactNode,
        children: menuItems(sub.find(child => child.type === DropdownMenuSubContent)?.props.children as React.ReactNode, close, key) }];
    }
    if (node.type === DropdownMenuItem || node.type === DropdownMenuRadioItem || node.type === DropdownMenuCheckboxItem) {
      const item = props as ItemProps;
      const checked = node.type === DropdownMenuRadioItem ? radio?.value === props.value : props.checked === true;
      return [{ key, disabled: item.disabled, className: cn(item.inset && "pl-8", item.className),
        ...{ role: node.type === DropdownMenuRadioItem ? "menuitemradio" : node.type === DropdownMenuCheckboxItem ? "menuitemcheckbox" : "menuitem", "aria-checked": node.type === DropdownMenuItem ? undefined : checked },
        label: <span className="flex items-center gap-2">{checked && (node.type === DropdownMenuRadioItem ? <Circle size={12} fill="currentColor" /> : <Check size={14} />)}{item.children}</span>,
                onClick: ({ domEvent }) => {
          item.onClick?.(domEvent as unknown as React.MouseEvent<HTMLDivElement>);
          if (domEvent.defaultPrevented) return;
          const selectEvent = new Event("select", { cancelable: true });
          item.onSelect?.(selectEvent);
          if (selectEvent.defaultPrevented) return;
          if (node.type === DropdownMenuRadioItem) radio?.onValueChange?.(props.value as string);
          if (node.type === DropdownMenuCheckboxItem) (props.onCheckedChange as ((checked: boolean) => void) | undefined)?.(!checked);
          close();
        } }];
    }
    if (node.type === DropdownMenuLabel) return [{ key, type: "group", label: <span className={props.className as string}>{props.children as React.ReactNode}</span>, children: [] }];
    // Existing menus contain div wrappers for provider groups and custom empty states.
    if (typeof node.type === "string" || node.type === DropdownMenuGroup || node.type === React.Fragment) {
      const nested = menuItems(props.children as React.ReactNode, close, key, radio);
      return nested.length ? nested : [{ key, disabled: true, label: node }];
    }
    return [{ key, disabled: true, label: node }];
  });
}
function DropdownMenu({ children, open: controlledOpen, defaultOpen = false, onOpenChange }: {
  children?: React.ReactNode; open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; modal?: boolean;
}) {
  const [internalOpen, setInternalOpen] = React.useState(defaultOpen);
  const open = controlledOpen ?? internalOpen;
  const setOpen = (next: boolean) => { if (controlledOpen === undefined) setInternalOpen(next); onOpenChange?.(next); };
  const nodes = elements(children);
  const trigger = nodes.find(node => node.type === DropdownMenuTrigger)?.props as React.ComponentProps<typeof DropdownMenuTrigger> | undefined;
  const content = nodes.find(node => node.type === DropdownMenuContent)?.props as React.ComponentProps<typeof DropdownMenuContent> | undefined;
  if (!trigger) return null;
  const { asChild, children: triggerChildren, ...triggerProps } = trigger;
  const vertical = content?.side === "top" ? "top" : "bottom";
  const placement = `${vertical}${content?.align === "end" ? "Right" : "Left"}` as "topRight" | "topLeft" | "bottomRight" | "bottomLeft";
  return <Dropdown open={open} onOpenChange={setOpen} trigger={["click"]} placement={placement}
    popupRender={() => <div className={content?.className} style={content?.style} onClick={content?.onClick}>
      <Menu selectable={false} items={menuItems(content?.children, () => setOpen(false))} style={{ maxHeight: "min(70vh, 480px)", overflowY: "auto" }} />
    </div>}>
    {asChild ? triggerChildren : <button type="button" {...triggerProps}>{triggerChildren}</button>}
  </Dropdown>;
}
export { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem, DropdownMenuCheckboxItem, DropdownMenuRadioItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuShortcut, DropdownMenuGroup, DropdownMenuPortal, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger, DropdownMenuRadioGroup };
