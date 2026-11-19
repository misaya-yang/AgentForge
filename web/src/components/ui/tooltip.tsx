import * as React from "react";
import * as Primitive from "@radix-ui/react-tooltip";
import { cn } from "@/lib/utils";
const TriggerContext = React.createContext<{ node: HTMLButtonElement | null; set: (node: HTMLButtonElement | null) => void }>({ node: null, set: () => {} });
const ProviderPresent = React.createContext(false);
function TooltipProvider(props: React.ComponentProps<typeof Primitive.Provider>) {
  return <ProviderPresent.Provider value={true}><Primitive.Provider {...props} /></ProviderPresent.Provider>;
}
function Tooltip(props: React.ComponentProps<typeof Primitive.Root>) {
  const [node, set] = React.useState<HTMLButtonElement | null>(null);
  const provided = React.useContext(ProviderPresent);
  const content = <TriggerContext.Provider value={{ node, set }}><Primitive.Root {...props} /></TriggerContext.Provider>;
  return provided ? content : <Primitive.Provider>{content}</Primitive.Provider>;
}
function TooltipTrigger({ ref, ...props }: React.ComponentProps<typeof Primitive.Trigger>) {
  const { set } = React.useContext(TriggerContext);
  const attach = React.useCallback((node: HTMLButtonElement | null) => { set(node); if (typeof ref === "function") return ref(node); if (ref) ref.current = node; }, [ref, set]);
  return <Primitive.Trigger {...props} ref={attach} type="button" />;
}
function TooltipContent({ className, sideOffset = 4, ...props }: React.ComponentProps<typeof Primitive.Content>) {
  const { node } = React.useContext(TriggerContext);
  return <Primitive.Portal container={node?.closest("dialog") ?? undefined}><Primitive.Content {...props} sideOffset={sideOffset} className={cn("forge-tooltip", className)} /></Primitive.Portal>;
}
export { TooltipProvider, Tooltip, TooltipTrigger, TooltipContent };
