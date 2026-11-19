import * as React from "react";
import { Switch as AntSwitch } from "antd";
interface SwitchProps extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "onChange"> {
  checked?: boolean; defaultChecked?: boolean; onCheckedChange?: (checked: boolean) => void; required?: boolean; value?: string;
}
const Switch = React.forwardRef<HTMLButtonElement, SwitchProps>(({
  onCheckedChange, onClick, value, defaultValue, name, required, checked, defaultChecked = false, ...props
}, ref) => {
  void defaultValue;
  const [internalChecked, setInternalChecked] = React.useState(defaultChecked);
  const selected = checked ?? internalChecked;
  return <>
    {name && <input type="checkbox" tabIndex={-1} aria-hidden="true" className="sr-only" name={name} value={value ?? "on"} checked={selected} required={required} disabled={props.disabled} readOnly />}
    <AntSwitch {...props} ref={ref} checked={selected} onClick={(_checked, event) => { if ("clientX" in event) onClick?.(event); }}
      onChange={next => { setInternalChecked(next); onCheckedChange?.(next); }} />
  </>;
});
Switch.displayName = "Switch";
export { Switch };
