import * as React from "react";
import * as Primitive from "@radix-ui/react-dropdown-menu";
import { Check, Circle } from "lucide-react";
import { cn } from "@/lib/utils";
const TriggerContext = React.createContext<{ node: HTMLButtonElement | null; set: (node: HTMLButtonElement | null) => void }>({ node: null, set: () => {} });
function DropdownMenu(props: React.ComponentProps<typeof Primitive.Root>) {
  const [node, set] = React.useState<HTMLButtonElement | null>(null);
  return <TriggerContext.Provider value={{ node, set }}><Primitive.Root {...props} /></TriggerContext.Provider>;
}
function DropdownMenuTrigger({ ref, ...props }: React.ComponentProps<typeof Primitive.Trigger>) {
  const { set } = React.useContext(TriggerContext);
  const attach = React.useCallback((node: HTMLButtonElement | null) => { set(node); if (typeof ref === "function") return ref(node); if (ref) ref.current = node; }, [ref, set]);
  return <Primitive.Trigger {...props} ref={attach} type="button" />;
}
function DropdownMenuPortal({ children }: { children?: React.ReactNode }) {
  const { node } = React.useContext(TriggerContext);
  return <Primitive.Portal container={node?.closest("dialog") ?? undefined}>{children}</Primitive.Portal>;
}
function DropdownMenuContent({ className, sideOffset = 4, ...props }: React.ComponentProps<typeof Primitive.Content>) {
  return <DropdownMenuPortal><Primitive.Content {...props} sideOffset={sideOffset} className={cn("forge-menu", className)} /></DropdownMenuPortal>;
}
function DropdownMenuItem({ className, inset, ...props }: React.ComponentProps<typeof Primitive.Item> & { inset?: boolean }) {
  return <Primitive.Item {...props} className={cn("forge-menu-item", inset && "pl-8", className)} />;
}
function DropdownMenuCheckboxItem({ className, children, inset, ...props }: React.ComponentProps<typeof Primitive.CheckboxItem> & { inset?: boolean }) {
  return <Primitive.CheckboxItem {...props} className={cn("forge-menu-item", inset && "pl-8", className)}><Primitive.ItemIndicator><Check size={14} aria-hidden="true" /></Primitive.ItemIndicator>{children}</Primitive.CheckboxItem>;
}
function DropdownMenuRadioItem({ className, children, inset, ...props }: React.ComponentProps<typeof Primitive.RadioItem> & { inset?: boolean }) {
  return <Primitive.RadioItem {...props} className={cn("forge-menu-item", inset && "pl-8", className)}><Primitive.ItemIndicator><Circle size={10} fill="currentColor" aria-hidden="true" /></Primitive.ItemIndicator>{children}</Primitive.RadioItem>;
}
function DropdownMenuLabel({ className, inset, ...props }: React.ComponentProps<typeof Primitive.Label> & { inset?: boolean }) { return <Primitive.Label {...props} className={cn("forge-menu-label", inset && "pl-8", className)} />; }
function DropdownMenuSeparator({ className, ...props }: React.ComponentProps<typeof Primitive.Separator>) { return <Primitive.Separator {...props} className={cn("forge-menu-separator", className)} />; }
function DropdownMenuShortcut({ className, ...props }: React.HTMLAttributes<HTMLSpanElement>) { return <span {...props} className={cn("ui-dropdown-menu-slot-1", className)} />; }
function DropdownMenuSubTrigger({ className, inset, ...props }: React.ComponentProps<typeof Primitive.SubTrigger> & { inset?: boolean }) { return <Primitive.SubTrigger {...props} className={cn("forge-menu-item", inset && "pl-8", className)} />; }
function DropdownMenuSubContent({ className, ...props }: React.ComponentProps<typeof Primitive.SubContent>) { return <Primitive.SubContent {...props} className={cn("forge-menu", className)} />; }
const DropdownMenuGroup = Primitive.Group;
const DropdownMenuSub = Primitive.Sub;
const DropdownMenuRadioGroup = Primitive.RadioGroup;
export { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem, DropdownMenuCheckboxItem, DropdownMenuRadioItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuShortcut, DropdownMenuGroup, DropdownMenuPortal, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger, DropdownMenuRadioGroup };
