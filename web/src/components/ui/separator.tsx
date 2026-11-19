import * as React from "react";
import { Divider } from "antd";
const Separator = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement> & { orientation?: "horizontal" | "vertical"; decorative?: boolean }>(
  ({ orientation = "horizontal", decorative = true, ...props }, ref) => <div {...props} ref={ref} role={decorative ? "none" : "separator"} aria-orientation={decorative ? undefined : orientation}>
    <Divider type={orientation} style={{ margin: 0 }} />
  </div>);
Separator.displayName = "Separator";
export { Separator };
