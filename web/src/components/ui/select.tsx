import * as React from "react";
import { Select as AntSelect } from "antd";
import { elements } from "./composition";
import { cn } from "@/lib/utils";

type SelectProps = {
  children?: React.ReactNode; value?: string; defaultValue?: string;
  onValueChange?: (value: string) => void; disabled?: boolean; name?: string;
  required?: boolean; open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void;
};
interface TriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> { ref?: React.Ref<HTMLButtonElement> }
function SelectTrigger(_props: TriggerProps) { void _props; return null; }
function SelectValue(_props: { placeholder?: React.ReactNode; children?: React.ReactNode }) { void _props; return null; }
function SelectContent(_props: React.HTMLAttributes<HTMLDivElement> & { disablePortal?: boolean; position?: string; align?: string; side?: string; sideOffset?: number }) { void _props; return null; }
function SelectItem(_props: React.HTMLAttributes<HTMLDivElement> & { value: string; disabled?: boolean; textValue?: string }) { void _props; return null; }

function Select({ children, value, defaultValue, onValueChange, name, required, onOpenChange, ...props }: SelectProps) {
  const nodes = elements(children);
  const trigger = nodes.find(node => node.type === SelectTrigger)?.props as TriggerProps | undefined;
  const content = nodes.find(node => node.type === SelectContent)?.props as React.ComponentProps<typeof SelectContent> | undefined;
  const placeholder = elements(trigger?.children).find(node => node.type === SelectValue)?.props.placeholder;
  const options = elements(content?.children).filter(node => node.type === SelectItem).map(node => ({
    value: node.props.value as string, label: node.props.children as React.ReactNode,
    disabled: node.props.disabled as boolean | undefined, title: node.props.textValue as string | undefined,
  }));
  const [internalValue, setInternalValue] = React.useState(defaultValue);
  const selected = value ?? internalValue;
  const selectRef = React.useRef<React.ComponentRef<typeof AntSelect>>(null);
  return <>
    {name && <input tabIndex={-1} aria-hidden="true" className="sr-only" name={name} value={selected ?? ""} required={required} disabled={props.disabled || trigger?.disabled} onChange={() => {}} onFocus={() => selectRef.current?.focus()} />}
    <AntSelect {...props} {...Object.fromEntries(Object.entries(trigger ?? {}).filter(([key]) => key.startsWith("data-") || key.startsWith("aria-") || key === "title"))} ref={selectRef} id={trigger?.id} aria-label={trigger?.["aria-label"]} aria-labelledby={trigger?.["aria-labelledby"]}
      aria-required={required} value={selected === "" ? undefined : selected} options={options}
      placeholder={placeholder as React.ReactNode} onChange={next => { setInternalValue(next); onValueChange?.(next); }}
      onOpenChange={onOpenChange} className={cn("w-full", trigger?.className)} style={trigger?.style}
      disabled={props.disabled || trigger?.disabled} classNames={{ popup: { root: content?.className } }}
      getPopupContainer={content?.disablePortal ? node => node.parentElement ?? document.body : undefined} />
  </>;
}

export { Select, SelectTrigger, SelectValue, SelectContent, SelectItem };
