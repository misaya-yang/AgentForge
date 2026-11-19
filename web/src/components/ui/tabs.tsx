import * as React from "react";
import * as Primitive from "@radix-ui/react-tabs";
import { elements } from "./composition";
import { cn } from "@/lib/utils";
const Selection = React.createContext<string | undefined>(undefined);
function Tabs({ value, defaultValue, onValueChange, children, ...props }: React.ComponentProps<typeof Primitive.Root>) {
  const [internal, setInternal] = React.useState(defaultValue);
  const firstValue = (children: React.ReactNode): string | undefined => {
    for (const node of elements(children)) {
      if (node.type === TabsTrigger) return node.props.value as string;
      if (node.type === TabsList || typeof node.type === "string") {
        const first = firstValue(node.props.children as React.ReactNode);
        if (first !== undefined) return first;
      }
    }
  };
  const selected = value ?? internal ?? firstValue(children);
  return <Selection.Provider value={selected}><Primitive.Root {...props} value={selected} onValueChange={next => { setInternal(next); onValueChange?.(next); }}>{children}</Primitive.Root></Selection.Provider>;
}
const TabsList = React.forwardRef<React.ComponentRef<typeof Primitive.List>, React.ComponentPropsWithoutRef<typeof Primitive.List>>(({ className, ...props }, ref) => <Primitive.List {...props} ref={ref} className={cn("forge-tabs-list", className)} />);
const TabsTrigger = React.forwardRef<React.ComponentRef<typeof Primitive.Trigger>, React.ComponentPropsWithoutRef<typeof Primitive.Trigger>>(({ className, ...props }, ref) => <Primitive.Trigger {...props} ref={ref} type="button" className={cn("forge-tabs-trigger", className)} />);
const TabsContent = React.forwardRef<React.ComponentRef<typeof Primitive.Content>, React.ComponentPropsWithoutRef<typeof Primitive.Content>>(({ className, ...props }, ref) => {
  const selected = React.useContext(Selection);
  const inactive = selected !== props.value;
  return <Primitive.Content {...props} ref={ref} hidden={inactive} aria-hidden={inactive || undefined} tabIndex={inactive ? -1 : 0} className={cn("forge-tabs-content", className)} />;
});
TabsList.displayName = "TabsList"; TabsTrigger.displayName = "TabsTrigger"; TabsContent.displayName = "TabsContent";
export { Tabs, TabsList, TabsTrigger, TabsContent };
