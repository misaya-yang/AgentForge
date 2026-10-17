import * as React from "react";
import { Alert } from "antd";
import { Button } from "./button";
import { createPortal } from "react-dom";

const ToastCloseContext = React.createContext(() => {});
interface ToastProps extends React.HTMLAttributes<HTMLDivElement> {
  variant?: "default" | "success" | "destructive" | "warning" | "info" | null;
  open?: boolean; onOpenChange?: (open: boolean) => void; duration?: number;
}
function ToastProvider({ children }: { children?: React.ReactNode }) {
  return createPortal(<div className="fixed right-4 top-4 z-[130] flex w-[calc(100vw-32px)] max-w-[420px] flex-col gap-2" aria-label="Notifications">{children}</div>, document.body);
}
function ToastViewport(_props: React.HTMLAttributes<HTMLDivElement>) { void _props; return null; }
const Toast = React.forwardRef<HTMLDivElement, ToastProps>(({ open = true, onOpenChange, duration = 5000, variant, children, ...props }, ref) => {
  const [paused, setPaused] = React.useState(false);
  const swipeStart = React.useRef<number | null>(null);
  const remaining = React.useRef(duration);
  const callback = React.useRef(onOpenChange);
  React.useLayoutEffect(() => { callback.current = onOpenChange; });
  React.useEffect(() => { remaining.current = duration; }, [duration, open]);
  React.useEffect(() => {
    if (!open || paused || duration === Infinity || duration <= 0) return;
    const start = Date.now();
    const timer = setTimeout(() => callback.current?.(false), remaining.current);
    return () => { clearTimeout(timer); remaining.current -= Date.now() - start; };
  }, [open, paused, duration]);
  if (!open) return null;
  return <ToastCloseContext.Provider value={() => onOpenChange?.(false)}><div {...props} ref={ref}
    onPointerDown={event => { props.onPointerDown?.(event); if (event.pointerType === "touch") swipeStart.current = event.clientX; }}
    onPointerUp={event => { props.onPointerUp?.(event); if (swipeStart.current !== null && event.clientX - swipeStart.current > 80) onOpenChange?.(false); swipeStart.current = null; }}
    onPointerCancel={() => { swipeStart.current = null; }}
    onKeyDown={event => { props.onKeyDown?.(event); if (event.key === "Escape" && !event.defaultPrevented) onOpenChange?.(false); }}
    onMouseEnter={() => setPaused(true)} onMouseLeave={() => setPaused(false)} onFocus={() => setPaused(true)}
    onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget)) setPaused(false); }}>
    <Alert type={variant === "destructive" ? "error" : variant === "success" || variant === "warning" ? variant : "info"}
      showIcon description={children} role={variant === "destructive" ? "alert" : "status"} />
  </div></ToastCloseContext.Provider>;
});
Toast.displayName = "Toast";
function ToastTitle(props: React.HTMLAttributes<HTMLDivElement>) { return <div {...props} className={`font-semibold ${props.className ?? ""}`} />; }
function ToastDescription(props: React.HTMLAttributes<HTMLDivElement>) { return <div {...props} />; }
function ToastIcon(_props: Pick<ToastProps, "variant">) { void _props; return null; }
function ToastClose(props: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const close = React.useContext(ToastCloseContext);
  return <Button {...props} variant="ghost" aria-label="Close" onClick={event => { props.onClick?.(event); if (!event.defaultPrevented) close(); }}>×</Button>;
}
const ToastAction = React.forwardRef<HTMLButtonElement, React.ButtonHTMLAttributes<HTMLButtonElement> & { altText: string }>(({ altText, onClick, ...props }, ref) => {
  const close = React.useContext(ToastCloseContext);
  return <Button {...props} ref={ref} aria-label={altText} onClick={event => { onClick?.(event); if (!event.defaultPrevented) close(); }} />;
});
ToastAction.displayName = "ToastAction";
type ToastActionElement = React.ReactElement<React.ComponentProps<typeof ToastAction>>;
export { type ToastProps, type ToastActionElement, ToastProvider, ToastViewport, Toast, ToastTitle, ToastDescription, ToastClose, ToastAction, ToastIcon };
