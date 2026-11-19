import * as React from "react";

/** Read declarative adapter children, including conditional arrays and fragments. */
export function elements(children: React.ReactNode): React.ReactElement<Record<string, unknown>>[] {
  return React.Children.toArray(children).flatMap(child => {
    if (!React.isValidElement<Record<string, unknown>>(child)) return [];
    return child.type === React.Fragment ? elements(child.props.children as React.ReactNode) : [child];
  });
}

/** Preserve child handlers, cancellation and refs for the legacy asChild contract. */
export function childWithProps(children: React.ReactNode, props: Record<string, unknown>) {
  const child = React.Children.only(children) as React.ReactElement<Record<string, unknown>>;
  // Slot children identify the element to enhance; they are not replacement content.
  // Keep the selected child's own content, otherwise Button asChild nests a Link in itself.
  const merged: Record<string, unknown> = { ...child.props, ...props, children: child.props.children };
  for (const key of Object.keys(props)) {
    if (key.startsWith("on") && typeof child.props[key] === "function" && typeof props[key] === "function") {
      merged[key] = (event: { defaultPrevented?: boolean }) => {
        (child.props[key] as (event: unknown) => void)(event);
        if (!event.defaultPrevented) (props[key] as (event: unknown) => void)(event);
      };
    }
  }
  merged.ref = props.ref ?? child.props.ref;
  if (child.props.style || props.style) merged.style = { ...(child.props.style as React.CSSProperties), ...(props.style as React.CSSProperties) };
  merged.className = [child.props.className, props.className].filter(Boolean).join(" ");
  if (props.ref && child.props.ref) {
    merged.ref = (node: unknown) => {
      const cleanups = [child.props.ref, props.ref].map(ref => {
        if (typeof ref === "function") return ref(node);
        (ref as React.RefObject<unknown>).current = node;
      });
      return () => cleanups.forEach((cleanup, index) => {
        const ref = [child.props.ref, props.ref][index];
        if (typeof cleanup === "function") cleanup();
        else if (typeof ref === "function") ref(null);
        else (ref as React.RefObject<unknown>).current = null;
      });
    };
  }
  return React.cloneElement(child, merged);
}
